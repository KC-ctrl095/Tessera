//! Private Android speech, reached only through the app's own IPC bridge.
#[cfg(target_os = "android")]
use tessera_background::BackgroundExt;

#[tauri::command]
pub async fn voice_status(
    app: tauri::AppHandle,
    language: String,
) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    return app
        .background()
        .voice_call("voiceStatus", serde_json::json!({"language": language}))
        .await;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, language);
        Ok(serde_json::json!({"listen": false, "speak": false}))
    }
}

#[tauri::command]
pub async fn voice_listen(
    app: tauri::AppHandle,
    language: String,
    events: tauri::ipc::Channel<serde_json::Value>,
    operation: Option<String>,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    return app
        .background()
        .voice_call(
            "voiceListen",
            serde_json::json!({"language": language, "events": events, "operation": operation}),
        )
        .await;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, language, events, operation);
        Err("Private native recognition isn't available here".into())
    }
}

#[tauri::command]
pub async fn voice_say(
    app: tauri::AppHandle,
    language: String,
    text: String,
    operation: Option<String>,
) -> Result<(), String> {
    if text.chars().count() > 4000 {
        return Err("Speech text is too long".into());
    }
    #[cfg(target_os = "android")]
    return app
        .background()
        .voice_call(
            "voiceSay",
            serde_json::json!({"language": language, "text": text, "operation": operation}),
        )
        .await;
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, language, text, operation);
        Err("Private native speech isn't available here".into())
    }
}

macro_rules! action {
    ($name:ident, $command:literal) => {
        #[tauri::command]
        pub async fn $name(app: tauri::AppHandle, operation: Option<String>) -> Result<(), String> {
            #[cfg(target_os = "android")]
            return app.background().voice_call($command, serde_json::json!({"operation": operation})).await;
            #[cfg(not(target_os = "android"))]
            {
                let _ = (app, operation);
                Ok(())
            }
        }
    };
}
action!(voice_finish, "voiceFinish");
action!(voice_cancel, "voiceCancel");
action!(voice_quiet, "voiceQuiet");
