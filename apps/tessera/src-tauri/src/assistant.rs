//! The exported Android assist entry carries no chat content or authority.
#[cfg(target_os = "android")]
use tessera_background::BackgroundExt;

#[tauri::command]
pub async fn assistant_watch(
    app: tauri::AppHandle,
    events: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    return app
        .background()
        .voice_call(
            "registerListener",
            serde_json::json!({"event": "assistant", "handler": events}),
        )
        .await;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, events);
        Ok(())
    }
}

/// A waiting assist request, and whether the system sent it (then listening
/// starts by itself; another app's only opens the chat).
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Pending {
    pending: bool,
    #[serde(default)]
    listen: bool,
}

#[tauri::command]
pub async fn assistant_take_pending(app: tauri::AppHandle) -> Result<Pending, String> {
    #[cfg(target_os = "android")]
    {
        app.background()
            .voice_call("assistantTakePending", ())
            .await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(Pending::default())
    }
}
