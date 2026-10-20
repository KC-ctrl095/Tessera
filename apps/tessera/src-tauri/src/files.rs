//! Files from a chat (shared by an agent, downloaded in a web view, or an
//! upload) saved on this device: into Android's Downloads, or where the
//! user picks in the save dialog on a computer.
//!
//! The file comes over the app's own (end-to-end encrypted) connection in
//! pieces, a few at a time. Only files saved here can be opened from the
//! app afterwards.
//!
//! A file can also be opened straight away with this computer's app for it
//! (`file_open_with`: a copy in the app's cache, no save dialog), and a text
//! file shown in a window of its own (`file_window`). Only known documents
//! and media open as they are; programs never do, and anything else opens as
//! text (see [`opens_as`]): a file in a chat may come from a model or a web
//! page.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use futures::StreamExt;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tessera_proto::{FileChunk, Request};

use crate::AppState;

/// One request's worth, and how many are in flight at once.
const CHUNK: u32 = 1 << 20;
const IN_FLIGHT: usize = 4;

/// What was saved where, so only those can be opened.
#[derive(Default)]
pub(crate) struct Saved(Mutex<HashSet<String>>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Downloaded {
    pub name: String,
    /// Where it was saved (a computer) or its `content://` address (Android).
    pub location: String,
    /// Shown to the user: "Downloads", or the folder.
    pub place: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    id: String,
    received: u64,
    size: u64,
}

fn client(state: &AppState) -> Result<tessera_client::Client, String> {
    state
        .conn
        .lock()
        .as_ref()
        .map(|(_, c)| c.clone())
        .ok_or_else(|| "Not connected to a server.".to_string())
}

/// A name that is safe as the last part of a path here.
fn safe_name(name: &str) -> String {
    let n: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .take(120)
        .collect();
    let n = n.trim().trim_start_matches('.').to_string();
    if n.is_empty() { "download".into() } else { n }
}

async fn read(client: &tessera_client::Client, id: &str, offset: u64) -> Result<FileChunk, String> {
    let v = client
        .call(Request::ReadSharedFile {
            id: id.to_string(),
            offset,
            len: CHUNK,
        })
        .await
        .map_err(|e| e.message)?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// Fetch file `id` into `to`, reporting progress.
async fn fetch(
    app: &AppHandle,
    client: &tessera_client::Client,
    id: &str,
    to: &Path,
) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt;
    let mut out = tokio::fs::File::create(to)
        .await
        .map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
    let first = read(client, id, 0).await?;
    let size = first.size;
    let progress = |received: u64| {
        let _ = app.emit(
            "tessera://download",
            Progress {
                id: id.to_string(),
                received,
                size,
            },
        );
    };
    let decode = |c: FileChunk| {
        base64::engine::general_purpose::STANDARD
            .decode(c.data)
            .map_err(|e| e.to_string())
    };
    let mut received = 0u64;
    let eof = first.eof;
    let bytes = decode(first)?;
    received += bytes.len() as u64;
    out.write_all(&bytes).await.map_err(|e| e.to_string())?;
    progress(received);
    if !eof {
        let offsets: Vec<u64> = (received..size).step_by(CHUNK as usize).collect();
        let mut chunks = futures::stream::iter(offsets)
            .map(|offset| read(client, id, offset))
            .buffered(IN_FLIGHT);
        while let Some(c) = chunks.next().await {
            let c = c?;
            if c.offset != received {
                return Err("the file changed on the server while it downloaded".into());
            }
            let bytes = decode(c)?;
            received += bytes.len() as u64;
            out.write_all(&bytes).await.map_err(|e| e.to_string())?;
            progress(received);
        }
    }
    out.flush().await.map_err(|e| e.to_string())?;
    out.sync_all().await.map_err(|e| e.to_string())?;
    if received != size {
        return Err("the download stopped early; try again".into());
    }
    Ok(size)
}

/// Download shared file `id` and save it on this device. A computer asks
/// where first (Linux) or saves to Downloads; a phone saves to Downloads.
#[tauri::command]
pub(crate) async fn file_download(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: String,
    mime: String,
) -> Result<Downloaded, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("not a file of this server".into());
    }
    let client = client(&state)?;
    let name = safe_name(&name);
    let _ = &mime;
    #[cfg(mobile)]
    {
        use tessera_background::BackgroundExt;
        let dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("downloads");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(&id);
        let fetched = fetch(&app, &client, &id, &tmp).await;
        let saved = match fetched {
            Ok(_) => {
                app.background()
                    .save_download(&tmp.display().to_string(), &name, &mime)
                    .await
            }
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_file(&tmp);
        let saved = saved?;
        app.state::<Saved>().0.lock().insert(saved.uri.clone());
        Ok(Downloaded {
            name: saved.name,
            location: saved.uri,
            place: "Downloads".into(),
        })
    }
    #[cfg(desktop)]
    {
        let Some(to) = save_target(&app, &name).await else {
            return Err("cancelled".into());
        };
        let part = to.with_file_name(format!(
            ".{}.part",
            to.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download")
        ));
        match fetch(&app, &client, &id, &part).await {
            Ok(_) => std::fs::rename(&part, &to).map_err(|e| {
                let _ = std::fs::remove_file(&part);
                format!("couldn't save {}: {e}", to.display())
            })?,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        }
        let location = to.display().to_string();
        app.state::<Saved>().0.lock().insert(location.clone());
        Ok(Downloaded {
            name: to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(name),
            // "Downloads", not the whole path.
            place: to
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            location,
        })
    }
}

/// The most the page may hand over to save in one go (a note as Markdown).
const SAVE_MAX: usize = 32 << 20;

/// Save something the page made (a note as Markdown, base64 `data`) on this
/// device, like a download: a phone into Downloads, a computer where the
/// user picks (Linux) or into Downloads.
#[tauri::command]
pub(crate) async fn file_save(
    app: AppHandle,
    name: String,
    mime: String,
    data: String,
) -> Result<Downloaded, String> {
    if data.len() > SAVE_MAX / 3 * 4 + 4 {
        return Err("that is too big to save from here".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| e.to_string())?;
    let name = safe_name(&name);
    let _ = &mime;
    #[cfg(mobile)]
    {
        use tessera_background::BackgroundExt;
        let dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("downloads");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("save-{}", std::process::id()));
        let saved = match tokio::fs::write(&tmp, &bytes).await {
            Ok(()) => {
                app.background()
                    .save_download(&tmp.display().to_string(), &name, &mime)
                    .await
            }
            Err(e) => Err(e.to_string()),
        };
        let _ = std::fs::remove_file(&tmp);
        let saved = saved?;
        app.state::<Saved>().0.lock().insert(saved.uri.clone());
        Ok(Downloaded {
            name: saved.name,
            location: saved.uri,
            place: "Downloads".into(),
        })
    }
    #[cfg(desktop)]
    {
        let Some(to) = save_target(&app, &name).await else {
            return Err("cancelled".into());
        };
        tokio::fs::write(&to, &bytes)
            .await
            .map_err(|e| format!("couldn't save {}: {e}", to.display()))?;
        let location = to.display().to_string();
        app.state::<Saved>().0.lock().insert(location.clone());
        Ok(Downloaded {
            name: to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(name),
            place: to
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            location,
        })
    }
}

/// Open a file saved here (`reveal`: show it in its folder instead).
#[tauri::command]
pub(crate) async fn file_open(
    app: AppHandle,
    location: String,
    mime: Option<String>,
    reveal: Option<bool>,
) -> Result<(), String> {
    if !app.state::<Saved>().0.lock().contains(&location) {
        return Err("Only files saved from Tessera can be opened here.".into());
    }
    let _ = (&mime, &reveal);
    #[cfg(mobile)]
    {
        use tessera_background::BackgroundExt;
        app.background()
            .open_download(&location, mime.as_deref().unwrap_or("*/*"))
            .await
    }
    #[cfg(desktop)]
    {
        use tauri_plugin_opener::OpenerExt;
        // Showing it in its folder is always fine.
        if reveal.unwrap_or(false) {
            return app
                .opener()
                .reveal_item_in_dir(&location)
                .map_err(|e| e.to_string());
        }
        // Opening it: as [`opens_as`] says (a phone's app chooser goes by the
        // file's type and runs nothing; Android has no such rule to apply).
        let path = match opens_as(&location) {
            Opens::Never => return Err(NEVER_OPENED.into()),
            Opens::Shown => location,
            Opens::AsText => text_copy(&app, &location)?,
        };
        app.opener()
            .open_path(&path, None::<&str>)
            .map_err(|e| e.to_string())
    }
}

/// A copy of a saved file named `….txt` in the app's cache, so it opens as
/// text (its own name could make the system run it).
#[cfg(desktop)]
fn text_copy(app: &AppHandle, from: &str) -> Result<String, String> {
    let meta = std::fs::metadata(from).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("That isn't a file.".into());
    }
    if meta.len() > VIEW_MAX as u64 {
        return Err(
            "Tessera opens this kind of file only as text, and it is too big for that. Use Show in folder."
                .into(),
        );
    }
    let name = Path::new(from)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = open_root(app)?.join(format!("t{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let to = dir.join(format!("{}.txt", safe_name(&name)));
    std::fs::copy(from, &to).map_err(|e| e.to_string())?;
    Ok(to.display().to_string())
}

/// Where copies opened with this computer's apps go (`open/` in the app's
/// cache), emptied of copies older than a day.
#[cfg(desktop)]
fn open_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("open");
    prune(&root);
    Ok(root)
}

/// What opening a file with this device's app would do.
#[cfg_attr(mobile, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opens {
    /// A known document or media file: its app shows it.
    Shown,
    /// Anything else (code, scripts, web pages, unknown kinds): opened as
    /// text, with `.txt` added, so the system can't run it.
    AsText,
    /// A program or installer: never opened from Tessera.
    Never,
}

/// Said when a program is not opened.
#[cfg_attr(mobile, allow(dead_code))]
const NEVER_OPENED: &str = "Tessera doesn't open programs or installers. Download it and open it yourself if you trust it.";

/// Programs, installers and shortcuts that systems run when they are opened.
#[cfg_attr(mobile, allow(dead_code))]
#[rustfmt::skip]
const PROGRAMS: &[&str] = &[
    "exe", "msi", "msix", "msixbundle", "appx", "appxbundle", "msp", "com", "scr", "pif", "cpl",
    "dll", "sys", "drv", "ocx", "lnk", "url", "website", "appref-ms", "application", "gadget",
    "hta", "reg", "inf", "scf", "msc", "diagcab", "settingcontent-ms", "library-ms", "jar",
    "jnlp", "app", "pkg", "mpkg", "dmg", "deb", "rpm", "apk", "aab", "xapk", "appimage", "snap",
    "flatpak", "flatpakref", "flatpakrepo", "run", "bin", "elf", "out", "desktop", "workflow",
    "action", "scpt", "xpi", "crx", "pyc", "pyo", "class",
];

/// Documents and media that open as they are (their apps show them, they
/// don't run anything). Everything else opens as text: an allow-list, since a
/// list of what's dangerous always misses something (help files, add-ins,
/// disk images that mount, documents with macros, web pages and pictures
/// with scripts in the default browser…).
#[cfg_attr(mobile, allow(dead_code))]
#[rustfmt::skip]
const SHOWN: &[&str] = &[
    "txt", "md", "log", "csv", "tsv", "json", "yaml", "yml", "toml", "ini", "conf", "xml", "pdf",
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "heic", "mp3", "wav", "ogg", "flac", "m4a",
    "mp4", "webm", "mkv", "mov", "docx", "xlsx", "pptx", "odt", "ods", "odp", "rtf", "epub",
];

/// What opening `name` would do, by the last extension of its own name
/// (none: as text).
#[cfg_attr(mobile, allow(dead_code))]
pub(crate) fn opens_as(name: &str) -> Opens {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    // Windows drops dots and spaces at the end of a name: "x.exe." is x.exe.
    let base = base.trim_end_matches(['.', ' ']);
    let Some((_, ext)) = base.rsplit_once('.') else {
        return Opens::AsText;
    };
    let ext = ext.to_ascii_lowercase();
    if PROGRAMS.contains(&ext.as_str()) {
        Opens::Never
    } else if SHOWN.contains(&ext.as_str()) {
        Opens::Shown
    } else {
        Opens::AsText
    }
}

/// The most the page may hand over for a window or to open (an attached
/// file's text), and the biggest saved file opened as text.
#[cfg_attr(mobile, allow(dead_code))]
const VIEW_MAX: usize = 16 << 20;

/// Open a file from a chat with this computer's app for it, without asking
/// where to save it: shared file `id` (fetched over the app's connection), or
/// `data` the page already has (base64: an attached file's text). The copy
/// goes into the app's cache (`open/`, emptied of copies older than a day)
/// and is opened from there. Only known documents and media keep their name;
/// anything else gets `.txt` added so it opens as text; programs are refused
/// (see [`opens_as`]).
#[cfg(desktop)]
#[tauri::command]
pub(crate) async fn file_open_with(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Option<String>,
    name: String,
    data: Option<String>,
) -> Result<Downloaded, String> {
    use tauri_plugin_opener::OpenerExt;
    let mut name = safe_name(&name);
    match opens_as(&name) {
        Opens::Never => return Err(NEVER_OPENED.into()),
        Opens::AsText => name.push_str(".txt"),
        Opens::Shown => {}
    }
    let root = open_root(&app)?;
    let to = match (id.as_deref(), data) {
        (Some(id), _) => {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("not a file of this server".into());
            }
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let to = dir.join(&name);
            // A shared file never changes: a copy from before is the same file.
            if !to.is_file() {
                let client = client(&state)?;
                let part = dir.join(".download.part");
                match fetch(&app, &client, id, &part).await {
                    Ok(_) => std::fs::rename(&part, &to).map_err(|e| e.to_string())?,
                    Err(e) => {
                        let _ = std::fs::remove_file(&part);
                        return Err(e);
                    }
                }
            }
            to
        }
        (None, Some(data)) => {
            if data.len() > VIEW_MAX / 3 * 4 + 4 {
                return Err("that is too big to open from here".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_bytes())
                .map_err(|e| e.to_string())?;
            let dir = root.join(format!("t{:016x}", rand::random::<u64>()));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let to = dir.join(&name);
            tokio::fs::write(&to, &bytes)
                .await
                .map_err(|e| e.to_string())?;
            to
        }
        (None, None) => return Err("nothing to open".into()),
    };
    let location = to.display().to_string();
    app.state::<Saved>().0.lock().insert(location.clone());
    app.opener()
        .open_path(&location, None::<&str>)
        .map_err(|e| e.to_string())?;
    Ok(Downloaded {
        name,
        location,
        place: String::new(),
    })
}

/// Copies opened more than a day ago go (their apps have long read them).
#[cfg(desktop)]
fn prune(root: &Path) {
    let Ok(list) = std::fs::read_dir(root) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in list.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Text files shown in windows of their own, by window label (`file-…`),
/// while the window is open.
#[derive(Default)]
pub(crate) struct Views(Mutex<std::collections::HashMap<String, View>>);

/// What a file window shows.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct View {
    pub name: String,
    pub mime: String,
    pub text: String,
}

/// Show a text file in a window of its own (the app's own page, which asks
/// for it with [`file_window_take`]). Nothing is written to disk. Answers
/// `true` (an app without this command answers nothing).
#[cfg(desktop)]
#[tauri::command]
pub(crate) async fn file_window(
    app: AppHandle,
    views: State<'_, Views>,
    name: String,
    mime: Option<String>,
    text: String,
) -> Result<bool, String> {
    if text.len() > VIEW_MAX {
        return Err("that is too big for a window; download it instead".into());
    }
    let label = format!("file-{:016x}", rand::random::<u64>());
    let title: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    views.0.lock().insert(
        label.clone(),
        View {
            name,
            mime: mime.unwrap_or_default(),
            text,
        },
    );
    let built =
        tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
            .title(format!("{title} — Tessera"))
            .inner_size(900.0, 720.0)
            .min_inner_size(360.0, 300.0)
            .background_color(tauri::window::Color(11, 11, 12, 255))
            .disable_drag_drop_handler()
            .build();
    if let Err(e) = built {
        views.0.lock().remove(&label);
        return Err(e.to_string());
    }
    Ok(true)
}

/// The file this window shows (none for other windows).
#[tauri::command]
pub(crate) fn file_window_take(
    window: tauri::WebviewWindow,
    views: State<'_, Views>,
) -> Option<View> {
    views.0.lock().get(window.label()).cloned()
}

/// A file window closed: its text goes.
pub(crate) fn window_closed(app: &AppHandle, label: &str) {
    if let Some(v) = app.try_state::<Views>() {
        v.0.lock().remove(label);
    }
}

/// The main window closed: its file windows go with it (else the app would
/// stay open with only them).
#[cfg(desktop)]
pub(crate) fn close_windows(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label.starts_with("file-") {
            let _ = w.destroy();
        }
    }
}

/// `name` in `dir`, or `name (1)`… if taken.
#[cfg_attr(any(mobile, target_os = "linux"), allow(dead_code))]
fn unique(dir: &Path, name: &str) -> PathBuf {
    let mut p = dir.join(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut n = 1;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

/// Linux: the system's save dialog (the desktop portal where there is one).
#[cfg(target_os = "linux")]
async fn save_target(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let window = app.get_webview_window("main")?;
    let folder = downloads_folder(app);
    let name = name.to_string();
    app.run_on_main_thread(move || {
        use gtk::prelude::*;
        let parent = window.gtk_window().ok();
        let dialog = gtk::FileChooserNative::new(
            Some("Save file"),
            parent.as_ref(),
            gtk::FileChooserAction::Save,
            Some("Save"),
            Some("Cancel"),
        );
        dialog.set_do_overwrite_confirmation(true);
        if let Some(f) = folder.filter(|f| f.is_dir()) {
            let _ = dialog.set_current_folder(f);
        }
        dialog.set_current_name(&name);
        let path = (dialog.run() == gtk::ResponseType::Accept)
            .then(|| dialog.filename())
            .flatten();
        dialog.destroy();
        let _ = tx.send(path);
    })
    .ok()?;
    rx.await.ok().flatten()
}

/// The user's Downloads folder (`~/Downloads` where the desktop doesn't
/// name one), or their home.
#[cfg(desktop)]
fn downloads_folder(app: &AppHandle) -> Option<PathBuf> {
    app.path().download_dir().ok().or_else(|| {
        let home = app.path().home_dir().ok()?;
        let d = home.join("Downloads");
        Some(if d.is_dir() { d } else { home })
    })
}

/// Other computers: straight to Downloads.
#[cfg(all(desktop, not(target_os = "linux")))]
async fn save_target(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let dir = downloads_folder(app)?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(unique(&dir, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(safe_name("../../.bashrc"), "bashrc");
        assert_eq!(safe_name("a/b\\c:d?.txt"), "cd.txt");
        assert_eq!(safe_name(" "), "download");
        assert_eq!(safe_name("report (final).pdf"), "report (final).pdf");
    }

    #[test]
    fn only_known_documents_open_as_they_are() {
        // Programs and installers: never.
        for n in [
            "setup.exe",
            "Install.MSI",
            "run.AppImage",
            "a.desktop",
            "x.lnk",
            "b.jar",
            "c.apk",
            "report.pdf.exe",
            "setup.exe.",
            "setup.exe. ",
            "setup.exe . .",
        ] {
            assert_eq!(opens_as(n), Opens::Never, "{n:?}");
        }
        // Known documents and media: as they are (any case, after Windows' trailing dots and spaces).
        for n in [
            "notes.txt",
            "report.pdf",
            "data.csv",
            "photo.JPG",
            "page.md",
            "/tmp/x/y.log",
            "C:\\Users\\me\\Downloads\\deck.pptx",
            "clip.mkv",
            "song.flac",
            "report.pdf.",
            "report.pdf ",
            "IMG_0001.HEIC",
            "book.epub",
        ] {
            assert_eq!(opens_as(n), Opens::Shown, "{n:?}");
        }
        // Everything else as text: scripts, code, web pages, pictures that can hold scripts, help
        // files, add-ins, remote desktop and sandbox files, disk images that mount when opened,
        // web queries, search connectors, documents with macros, archives, no extension at all.
        for n in [
            "fix.bat",
            "deploy.sh",
            "tool.ps1",
            "main.py",
            "app.js",
            "a.vbs",
            "lib.rs",
            "index.html",
            "page.htm",
            "logo.svg",
            "manual.chm",
            "addin.xll",
            "work.rdp",
            "disk.iso",
            "disk.img",
            "disk.vhd",
            "disk.vhdx",
            "box.wsb",
            "query.iqy",
            "sheet.slk",
            "find.search-ms",
            "find.searchConnector-ms",
            "budget.docm",
            "budget.xlsm",
            "budget.pptm",
            "archive.tar.gz",
            "archive.zip",
            "README",
            ".bashrc",
            "Makefile",
            "unknown.xyz",
            "report.pdf.html",
        ] {
            assert_eq!(opens_as(n), Opens::AsText, "{n:?}");
        }
        // The extension that counts is the last one, of the file's own name.
        assert_eq!(opens_as("archive.exe/readme.txt"), Opens::Shown);
        assert_eq!(opens_as("docs.pdf\\run"), Opens::AsText);
        assert_eq!(opens_as("readme.txt.exe"), Opens::Never);
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("tessera-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        std::fs::write(dir.join("a (1).txt"), b"").unwrap();
        assert_eq!(unique(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(unique(&dir, "b"), dir.join("b"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
