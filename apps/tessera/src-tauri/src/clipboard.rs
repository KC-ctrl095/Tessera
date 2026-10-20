//! What a paste or drop into the composer brings that the page can't see, in
//! the desktop app.
//!
//! WebKitGTK gives the page no image data when an image is pasted (the paste
//! event is empty), and neither the files nor their paths when files copied
//! in a file manager are pasted, or dragged in from one. The UI asks here
//! while it handles that paste or drop:
//!
//! * `clipboard_image` reads this display's clipboard through GTK (Wayland or
//!   X11) and returns the picture as PNG.
//! * `clipboard_files` reads the files the clipboard names (`text/uri-list`,
//!   or GNOME's `x-special/gnome-copied-files` that Thunar and Nautilus use).
//! * `dropped_files` reads the files of the last drag from another app over
//!   the window (the shell keeps the links WebKitGTK received, see
//!   [`watch_drops`]).
//!
//! Only local `file://` links are followed, only regular files are read, at
//! most [`MAX_FILES`] of at most [`MAX_FILE_BYTES`] each (the composer's own
//! limits). Nothing is written to disk or kept. Only the app's main frame can
//! call these (board frames get no IPC bridge, see vendor/README.md).

use serde::Serialize;

/// The composer's limits (`MAX_FILES`, `MAX_FILE_BYTES` in ui/src/lib/images.ts).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const MAX_FILES: usize = 10;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const MAX_FILE_BYTES: u64 = 25 << 20;

/// A pasted picture, base64 PNG.
#[derive(Debug, Clone, Serialize)]
pub struct ClipImage {
    pub mime: String,
    pub data: String,
    pub width: u32,
    pub height: u32,
}

/// Files a paste or drop named, read for the composer.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PickedFiles {
    pub files: Vec<PickedFile>,
    /// Named but not read: `folder`, `too large`, `unreadable` or `limit`
    /// (more than [`MAX_FILES`]).
    pub skipped: Vec<Skipped>,
}

/// A file's name, type and contents (base64).
#[derive(Debug, Clone, Serialize)]
pub struct PickedFile {
    pub name: String,
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub name: String,
    pub reason: &'static str,
}

/// The image on the clipboard, or `None` when it holds none (or on platforms
/// whose web view hands pasted images to the page itself).
#[tauri::command]
pub async fn clipboard_image(window: tauri::WebviewWindow) -> Result<Option<ClipImage>, String> {
    #[cfg(target_os = "linux")]
    return linux::read(&window).await;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        Ok(None)
    }
}

/// The files copied in a file manager, read; none when the clipboard names
/// no files (or on platforms whose web view hands them to the page itself).
#[tauri::command]
pub async fn clipboard_files(window: tauri::WebviewWindow) -> Result<PickedFiles, String> {
    #[cfg(target_os = "linux")]
    {
        let links = linux::copied_files(&window).await?;
        read_links(links).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        Ok(PickedFiles::default())
    }
}

/// The files of the drag that was just dropped on the window, read (once).
#[tauri::command]
pub async fn dropped_files() -> Result<PickedFiles, String> {
    #[cfg(target_os = "linux")]
    return read_links(linux::take_drop()).await;
    #[cfg(not(target_os = "linux"))]
    {
        Ok(PickedFiles::default())
    }
}

/// Remember the file links of drags from other apps over the window, for
/// [`dropped_files`].
#[cfg(target_os = "linux")]
pub fn watch_drops(window: &tauri::WebviewWindow) {
    linux::watch_drops(window);
}

/// The local paths among `file://` links (others, such as web links, are left out).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn file_links(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("file://"))
        .map(String::from)
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
async fn read_links(links: Vec<String>) -> Result<PickedFiles, String> {
    if links.is_empty() {
        return Ok(PickedFiles::default());
    }
    tauri::async_runtime::spawn_blocking(move || read_files(links))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
fn read_files(links: Vec<String>) -> PickedFiles {
    use base64::Engine as _;
    use std::io::Read as _;
    let mut out = PickedFiles::default();
    for link in links {
        let Some(path) = linux::local_path(&link) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let skip = |reason| Skipped {
            name: name.clone(),
            reason,
        };
        if out.files.len() >= MAX_FILES {
            out.skipped.push(skip("limit"));
            continue;
        }
        // Follows links; a folder, device or pipe is never opened.
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => out.skipped.push(skip("folder")),
            Ok(m) if !m.is_file() => out.skipped.push(skip("unreadable")),
            Ok(m) if m.len() > MAX_FILE_BYTES => out.skipped.push(skip("too large")),
            Ok(_) => {
                let mut bytes = Vec::new();
                let read = std::fs::File::open(&path)
                    .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
                match read {
                    Ok(_) if bytes.len() as u64 > MAX_FILE_BYTES => {
                        out.skipped.push(skip("too large"))
                    }
                    Ok(_) => out.files.push(PickedFile {
                        mime: linux::mime_of(&path, &bytes),
                        name,
                        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    }),
                    Err(_) => out.skipped.push(skip("unreadable")),
                }
            }
            Err(_) => out.skipped.push(skip("unreadable")),
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn read_files(_links: Vec<String>) -> PickedFiles {
    PickedFiles::default()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::ClipImage;
    use base64::Engine as _;
    use gdk::gdk_pixbuf::{Colorspace, Pixbuf};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// Larger pictures are refused (the UI scales what it gets down to 2048 px).
    const MAX_PIXELS: u64 = 64 << 20;
    /// A drop is read within this long of the drag reaching the window.
    const DROP_WINDOW: Duration = Duration::from_secs(120);

    /// The file links of the last drag from another app, and when they came.
    static LAST_DRAG: parking_lot::Mutex<Option<(Instant, Vec<String>)>> =
        parking_lot::Mutex::new(None);

    /// A picture's pixels, copied out on the main thread (a `Pixbuf` stays there).
    struct Raw {
        bytes: gdk::glib::Bytes,
        alpha: bool,
        width: i32,
        height: i32,
        stride: i32,
    }

    pub async fn read(window: &tauri::WebviewWindow) -> Result<Option<ClipImage>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Option<Raw>, String>>();
        window
            .run_on_main_thread(move || {
                let clipboard = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                clipboard.request_image(move |_, pixbuf| {
                    let raw = pixbuf.map(|p| {
                        let (w, h) = (p.width().max(0), p.height().max(0));
                        if u64::from(w.unsigned_abs()) * u64::from(h.unsigned_abs()) > MAX_PIXELS {
                            return Err("the picture is too large".to_string());
                        }
                        Ok(Raw {
                            bytes: p.read_pixel_bytes(),
                            alpha: p.has_alpha(),
                            width: w,
                            height: h,
                            stride: p.rowstride(),
                        })
                    });
                    let _ = tx.send(raw.transpose());
                });
            })
            .map_err(|e| e.to_string())?;
        let raw = answer(rx).await??;
        let Some(raw) = raw else { return Ok(None) };
        tauri::async_runtime::spawn_blocking(move || encode(raw))
            .await
            .map_err(|e| e.to_string())?
            .map(Some)
    }

    /// The clipboard's file links: a `text/uri-list` (most file managers), else
    /// `x-special/gnome-copied-files` ("copy" or "cut", then the links).
    pub async fn copied_files(window: &tauri::WebviewWindow) -> Result<Vec<String>, String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Vec<String>>();
        window
            .run_on_main_thread(move || {
                let clipboard = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
                let uri_list = gdk::Atom::intern("text/uri-list");
                clipboard.request_contents(&uri_list, move |clipboard, data| {
                    let links = links_in(data);
                    if !links.is_empty() {
                        let _ = tx.send(links);
                        return;
                    }
                    let gnome = gdk::Atom::intern("x-special/gnome-copied-files");
                    clipboard.request_contents(&gnome, move |_, data| {
                        let _ = tx.send(links_in(data));
                    });
                });
            })
            .map_err(|e| e.to_string())?;
        answer(rx).await
    }

    fn links_in(data: &gtk::SelectionData) -> Vec<String> {
        if data.length() <= 0 {
            return Vec::new();
        }
        super::file_links(&String::from_utf8_lossy(&data.data()))
    }

    async fn answer<T>(rx: tokio::sync::oneshot::Receiver<T>) -> Result<T, String> {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "the clipboard didn't answer".to_string())?
            .map_err(|_| "the clipboard read was dropped".to_string())
    }

    pub fn watch_drops(window: &tauri::WebviewWindow) {
        let _ = window.with_webview(|pw| {
            use gtk::prelude::WidgetExt;
            // WebKitGTK asks for the drag's data when it enters the window; it
            // keeps file paths to itself, so note them here too. A drag whose
            // links are no local files (a web link, the page's own drags)
            // clears what an earlier one left.
            pw.inner()
                .connect_drag_data_received(|_, _, _, _, data, _, _| {
                    if data.target().name().as_str() != "text/uri-list" {
                        return;
                    }
                    let links: Vec<String> = data
                        .uris()
                        .iter()
                        .filter(|u| u.starts_with("file://"))
                        .map(|u| u.to_string())
                        .collect();
                    *LAST_DRAG.lock() = (!links.is_empty()).then(|| (Instant::now(), links));
                });
        });
    }

    pub fn take_drop() -> Vec<String> {
        LAST_DRAG
            .lock()
            .take()
            .filter(|(at, _)| at.elapsed() < DROP_WINDOW)
            .map(|(_, links)| links)
            .unwrap_or_default()
    }

    /// A `file://` link's path on this computer (`None` for another host's).
    pub fn local_path(link: &str) -> Option<PathBuf> {
        let (path, host) = gdk::glib::filename_from_uri(link).ok()?;
        match host.as_deref() {
            None | Some("") | Some("localhost") => Some(path),
            Some(h) if h == gdk::glib::host_name().as_str() => Some(path),
            Some(_) => None,
        }
    }

    pub fn mime_of(path: &Path, bytes: &[u8]) -> String {
        let head = &bytes[..bytes.len().min(4096)];
        let (kind, _) = gtk::gio::content_type_guess(Some(path), head);
        gtk::gio::content_type_get_mime_type(&kind)
            .map(|m| m.to_string())
            .unwrap_or_else(|| "application/octet-stream".into())
    }

    fn encode(raw: Raw) -> Result<ClipImage, String> {
        let pixbuf = Pixbuf::from_bytes(
            &raw.bytes,
            Colorspace::Rgb,
            raw.alpha,
            8,
            raw.width,
            raw.height,
            raw.stride,
        );
        let png = pixbuf
            .save_to_bufferv("png", &[])
            .map_err(|e| e.to_string())?;
        Ok(ClipImage {
            mime: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(png),
            width: raw.width.unsigned_abs(),
            height: raw.height.unsigned_abs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_links_keep_local_links_only() {
        let uri_list = "# a comment\r\nfile:///home/me/a%20b.png\r\nhttps://example.org/c.png\r\n\r\nfile:///tmp/n.txt\r\n";
        assert_eq!(
            file_links(uri_list),
            ["file:///home/me/a%20b.png", "file:///tmp/n.txt"]
        );
        // GNOME's format: the operation first, then the links.
        assert_eq!(
            file_links("copy\nfile:///home/me/x.pdf"),
            ["file:///home/me/x.pdf"]
        );
        assert!(file_links("/home/me/x.pdf").is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_regular_files_within_limits() {
        let dir = std::env::temp_dir().join(format!("tessera-clip-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("notes one.txt"), "hello").unwrap();
        let link = |n: &str| format!("file://{}/{}", dir.display(), n.replace(' ', "%20"));
        let mut links = vec![link("notes one.txt"), link("folder"), link("missing.txt")];
        links.extend((0..MAX_FILES).map(|_| link("notes one.txt")));
        let got = read_files(links);
        assert_eq!(got.files.len(), MAX_FILES);
        assert_eq!(got.files[0].name, "notes one.txt");
        assert_eq!(got.files[0].mime, "text/plain");
        assert_eq!(got.files[0].data, "aGVsbG8=");
        let reasons: Vec<_> = got
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason))
            .collect();
        assert_eq!(
            reasons,
            [
                ("folder", "folder"),
                ("missing.txt", "unreadable"),
                ("notes one.txt", "limit")
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
