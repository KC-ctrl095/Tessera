//! The server offers bytes over the pinned encrypted connection. Nothing is
//! installed in the background: the native confirmation is always required.
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Check {
    server_id: String,
    build: Option<tessera_proto::AppUpdate>,
}

#[tauri::command]
pub(crate) fn app_update_available() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

#[tauri::command]
pub(crate) async fn app_update_check(
    state: State<'_, super::AppState>,
    app: tauri::AppHandle,
) -> Result<Option<Check>, String> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        let (server_id, client) = connection(&state, None)?;
        let build = offered(&client).await?;
        if let Some(b) = &build {
            local_version(&app, b)?;
        }
        Ok(Some(Check { server_id, build }))
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        let _ = (state, app);
        Ok(None)
    }
}

#[tauri::command]
pub(crate) async fn app_update_install(
    state: State<'_, super::AppState>,
    server_id: String,
    id: String,
    sha256: String,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        install(&state, &app, &server_id, &id, &sha256).await
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        let _ = (state, server_id, id, sha256, app);
        Err("App updates aren't available in this shell.".into())
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn local_version(app: &tauri::AppHandle, build: &tessera_proto::AppUpdate) -> Result<(), String> {
    use tauri::Manager;
    let receipt = app
        .path()
        .app_data_dir()
        .ok()
        .and_then(|d| std::fs::read(d.join("last-update-setup.json")).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let known_hash = receipt
        .as_ref()
        .filter(|r| {
            r["version"]
                .as_str()
                .and_then(|v| semver::Version::parse(v).ok())
                .is_some_and(|v| {
                    v.cmp_precedence(&semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap())
                        .is_eq()
                })
        })
        .and_then(|r| r["sha256"].as_str());
    super::update_checks::version(build, env!("CARGO_PKG_VERSION"), known_hash)
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn connection(
    state: &super::AppState,
    expected: Option<&str>,
) -> Result<(String, tessera_client::Client), String> {
    let (id, client) = state
        .conn
        .lock()
        .as_ref()
        .cloned()
        .ok_or("Connect to your server first.")?;
    if expected.is_some_and(|e| e != id) || !client.status().encrypted {
        return Err("The server connection changed; check for updates again.".into());
    }
    Ok((id, client))
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn still_current(
    state: &super::AppState,
    server: &str,
    client: &tessera_client::Client,
) -> Result<(), String> {
    let (_, current) = connection(state, Some(server))?;
    if !current.same_connection(client) {
        return Err("The server login changed; check for updates again.".into());
    }
    Ok(())
}

#[cfg(all(windows, target_arch = "x86_64"))]
async fn offered(
    client: &tessera_client::Client,
) -> Result<Option<tessera_proto::AppUpdate>, String> {
    let installed_at = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.created().or_else(|_| m.modified()).ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|t| t.as_millis() as u64);
    client
        .call_as(tessera_proto::Request::GetAppUpdate {
            platform: super::update_checks::PLATFORM.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            sha256: None,
            installed_at,
        })
        .await
        .map_err(|e| e.to_string())
}

#[cfg(all(windows, target_arch = "x86_64"))]
async fn install(
    state: &super::AppState,
    app: &tauri::AppHandle,
    server: &str,
    id: &str,
    hash: &str,
) -> Result<bool, String> {
    use super::update_checks;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, Write};
    use std::os::windows::fs::OpenOptionsExt;

    static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if BUSY.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return Err("An update is already in progress.".into());
    }
    struct Busy;
    impl Drop for Busy {
        fn drop(&mut self) {
            BUSY.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    let _busy = Busy;
    let (_, client) = connection(state, Some(server))?;
    let build = offered(&client)
        .await?
        .ok_or("This update is no longer offered; check again.")?;
    update_checks::offer(&build, id, hash)?;
    local_version(app, &build)?;
    let version = build.version.clone();
    let consent = tokio::task::spawn_blocking(move || {
        use windows::Win32::UI::WindowsAndMessaging::{
            IDCANCEL, IDOK, MB_DEFBUTTON2, MB_ICONWARNING, MB_OKCANCEL,
            MB_SETFOREGROUND, MB_TASKMODAL, MessageBoxW,
        };
        use windows::core::{HSTRING, w};
        let text = HSTRING::from(format!("Run the Windows setup offered by your connected server (version {version})? It will close Tessera and restart its server and machine. Your chats and settings stay. Only install builds you trust."));
        // SAFETY: the strings live through this synchronous native call. It
        // has no page-controlled command/path and Cancel is the default.
        let answer = unsafe {
            MessageBoxW(None, &text, w!("Install Tessera update?"),
                MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND | MB_TASKMODAL)
        };
        match answer {
            IDOK => Ok(true),
            IDCANCEL => Ok(false),
            _ => Err("The update confirmation couldn't be shown; nothing was run.".to_string()),
        }
    }).await.map_err(|_| "The update confirmation was interrupted; nothing was run.")??;
    if !consent {
        return Ok(false);
    }
    still_current(state, server, &client)?;
    let again = offered(&client)
        .await?
        .ok_or("This update is no longer offered; check again.")?;
    update_checks::offer(&again, id, hash)?;
    local_version(app, &again)?;
    if again.size != build.size {
        return Err("The update changed; check again.".into());
    }

    let temp = tempfile::Builder::new()
        .prefix("tessera-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let path = temp.path().join("tessera-setup.exe");
    // A new file in this account's unpredictable temp directory. Deny writes
    // and renames until after the loader starts; no shell or remote filename.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut offset = 0;
    while offset < build.size {
        let c = client
            .call_as(tessera_proto::Request::ReadAppUpdate {
                id: id.into(),
                offset,
                len: update_checks::CHUNK,
            })
            .await
            .map_err(|e| e.to_string())?;
        still_current(state, server, &client)?;
        let bytes = update_checks::chunk(c, offset, build.size)?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        offset += bytes.len() as u64;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    // Windows won't execute a file still open for writing. Reopen read-only,
    // deny new writers/renames, and verify those exact bytes under this hold.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
        .open(&path)
        .map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != build.size {
        return Err("The update file changed; nothing was run.".into());
    }
    file.rewind().map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    update_checks::hash(&actual, hash)?;
    file.rewind().map_err(|e| e.to_string())?;
    let mut prefix = Vec::new();
    (&mut file)
        .take(16 << 20)
        .read_to_end(&mut prefix)
        .map_err(|e| e.to_string())?;
    tessera_core::updates::validate_windows_installer(&prefix)?;
    still_current(state, server, &client)?;
    let refreshed = offered(&client)
        .await?
        .ok_or("The update is no longer offered.")?;
    update_checks::offer(&refreshed, id, hash)?;
    local_version(app, &refreshed)?;
    if refreshed.version != build.version || refreshed.size != build.size {
        return Err("The update changed; nothing was run.".into());
    }
    still_current(state, server, &client)?;
    tessera_core::platform::windows::keep_std_handles_private();
    let mut child = std::process::Command::new(&path)
        .current_dir(temp.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    drop(file);
    use tauri::Manager;
    let receipt_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("last-update-setup.json"));
    let receipt = serde_json::json!({"version": build.version, "sha256": build.sha256});
    // Setup may terminate this app. An absent success receipt is deliberately
    // unknown, not permission for same-version installation after cancellation.
    let dir = temp.keep();
    std::thread::spawn(move || {
        let _held_busy = _busy;
        if child.wait().is_ok_and(|status| status.success())
            && let Some(path) = receipt_path
        {
            let _ = super::private_file::write(&path, receipt.to_string().as_bytes());
        }
        let _ = std::fs::remove_dir_all(dir);
    });
    Ok(true)
}
