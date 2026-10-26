//! Tessera on Android: notifications posted natively, a foreground service
//! that keeps the server connection alive while work is running, and a
//! picture of one whiteboard tile when an agent asks to look at it.
//!
//! Only the app's Rust side calls this; nothing is exposed to the web UI.
//! Notifications are built in Kotlin so their intents are immutable, signed
//! and fixed to the app's own activity: nothing from the server ever becomes
//! an intent, only plain text and a checked session id.

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

/// One notification. A newer one with the same tag replaces it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    /// Server revision, for exact duplicate OS suppression across live and pull.
    pub notification_id: Option<u64>,
    /// A session id, or a fixed key for app-wide notifications.
    pub tag: String,
    /// Opened when the notification is tapped.
    pub session_id: Option<String>,
    /// Approvals: the heads-up channel. Everything else is quieter.
    pub urgent: bool,
    pub title: String,
    pub body: String,
    /// Shown instead of `body` on a secure lock screen.
    pub public_body: String,
    /// An agent's message (`notify`): its own channel, one notification
    /// each (the tag is its id), removed when its chat is opened.
    #[serde(default)]
    pub agent: bool,
    /// Quiet hours: no sound, vibration or pop-up.
    #[serde(default)]
    pub silent: bool,
    #[serde(default)]
    pub private: bool,
    /// Small text next to the app name (who sent it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// When it happened (unix ms), if not now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<u64>,
}

/// A downloaded file saved to the phone's Downloads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    /// `content://` address of the saved file (to open it).
    pub uri: String,
    /// The name it got (Android adds " (1)" to a taken name).
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `granted`, `denied` or `prompt`.
    pub permission: String,
    /// Doze and app standby apply (not exempted by the user).
    pub battery_optimized: bool,
}

/// Part of the web view to copy (CSS pixels of a viewport this big).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub viewport_width: f64,
    pub viewport_height: f64,
    /// Long side of the picture, at most.
    pub max_side: u32,
}

/// That part of the screen as an image (base64), never stored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub mime: String,
    pub data: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub running: bool,
    #[serde(default)]
    pub error: Option<String>,
}

pub struct Background<R: Runtime> {
    #[cfg(target_os = "android")]
    handle: tauri::plugin::PluginHandle<R>,
    #[cfg(not(target_os = "android"))]
    _runtime: std::marker::PhantomData<fn() -> R>,
}

#[cfg(target_os = "android")]
impl<R: Runtime> Background<R> {
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        args: impl Serialize,
    ) -> Result<T, String> {
        self.handle
            .run_mobile_plugin_async(command, args)
            .await
            .map_err(|e| e.to_string())
    }

    /// Only the app's own voice IPC commands call these native engines.
    pub async fn voice_call<T: serde::de::DeserializeOwned>(
        &self,
        command: &str,
        args: impl Serialize,
    ) -> Result<T, String> {
        self.call(command, args).await
    }

    pub async fn post(&self, post: &Post) -> Result<bool, String> {
        #[derive(Deserialize)]
        struct Posted {
            accepted: bool,
        }
        self.call::<Posted>("post", post)
            .await
            .map(|posted| posted.accepted)
    }

    /// Remove the notification for `tag`, or all of them.
    pub async fn withdraw(&self, tag: Option<&str>) -> Result<(), String> {
        self.call("withdraw", serde_json::json!({ "tag": tag }))
            .await
    }

    pub async fn status(&self) -> Result<Status, String> {
        self.call("status", ()).await
    }

    /// Show the system prompt if it may still be shown; returns the outcome.
    pub async fn request_permission(&self) -> Result<Status, String> {
        self.call("requestNotifications", ()).await
    }

    /// The system dialog that exempts the app from battery optimisation.
    pub async fn request_battery_exemption(&self) -> Result<Status, String> {
        self.call("requestBatteryExemption", ()).await
    }

    pub async fn open_settings(&self) -> Result<(), String> {
        self.call("openSettings", ()).await
    }

    /// `run`: keep the connection alive. `eager`: start now even with the
    /// app on screen; otherwise it starts when the app leaves the screen.
    pub async fn keep_alive(&self, run: bool, eager: bool) -> Result<Service, String> {
        self.call(
            "keepAlive",
            serde_json::json!({ "run": run, "eager": eager }),
        )
        .await
    }

    /// A notification was tapped (with a signature Kotlin checked), or the
    /// user chose "Disconnect" on the keep-alive notification.
    pub async fn listen(
        &self,
        on_open: impl Fn(String) + Send + Sync + 'static,
        on_disconnect: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), String> {
        use tauri::ipc::{Channel, InvokeResponseBody};
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Opened {
            session_id: String,
        }
        let open = Channel::<serde_json::Value>::new(move |body| {
            if let InvokeResponseBody::Json(json) = body
                && let Ok(o) = serde_json::from_str::<Opened>(&json)
            {
                on_open(o.session_id);
            }
            Ok(())
        });
        let disconnect = Channel::<serde_json::Value>::new(move |_| {
            on_disconnect();
            Ok(())
        });
        self.call::<()>(
            "registerListener",
            serde_json::json!({ "event": "open", "handler": open }),
        )
        .await?;
        self.call::<()>(
            "registerListener",
            serde_json::json!({ "event": "disconnect", "handler": disconnect }),
        )
        .await
    }

    /// Copy one rect of what the web view shows right now (a whiteboard
    /// tile the user is looking at), scaled to `max_side`.
    pub async fn capture(&self, rect: CaptureRect) -> Result<Capture, String> {
        self.call("capture", rect).await
    }

    /// Check for agents' messages in the background (every ~15 minutes
    /// while the app isn't on screen), with this app data directory.
    pub async fn configure_pull(&self, dir: &str, enabled: bool) -> Result<(), String> {
        self.call(
            "configurePull",
            serde_json::json!({ "dir": dir, "enabled": enabled }),
        )
        .await
    }

    /// Wake up at `at` (unix ms) to fetch a scheduled message.
    pub async fn wake_at(&self, at: u64) -> Result<(), String> {
        self.call("wakeAt", serde_json::json!({ "at": at })).await
    }

    /// Move a finished download (`path`, in the app's cache) into the
    /// phone's Downloads, and say so with a notification.
    pub async fn save_download(&self, path: &str, name: &str, mime: &str) -> Result<Saved, String> {
        self.call(
            "saveDownload",
            serde_json::json!({ "path": path, "name": name, "mime": mime }),
        )
        .await
    }

    /// Open a saved download with the app the user picks.
    pub async fn open_download(&self, uri: &str, mime: &str) -> Result<(), String> {
        self.call(
            "openDownload",
            serde_json::json!({ "uri": uri, "mime": mime }),
        )
        .await
    }

    /// The session of a tap that launched the app before `listen` was set up.
    pub async fn take_opened(&self) -> Result<Option<String>, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Taken {
            session_id: Option<String>,
        }
        Ok(self.call::<Taken>("takeOpened", ()).await?.session_id)
    }
}

/// iOS: not implemented yet; everything is a no-op.
#[cfg(not(target_os = "android"))]
impl<R: Runtime> Background<R> {
    pub async fn post(&self, _post: &Post) -> Result<bool, String> {
        Ok(false)
    }
    pub async fn withdraw(&self, _tag: Option<&str>) -> Result<(), String> {
        Ok(())
    }
    pub async fn status(&self) -> Result<Status, String> {
        Err("not supported on this platform".into())
    }
    pub async fn request_permission(&self) -> Result<Status, String> {
        self.status().await
    }
    pub async fn request_battery_exemption(&self) -> Result<Status, String> {
        self.status().await
    }
    pub async fn open_settings(&self) -> Result<(), String> {
        Ok(())
    }
    pub async fn keep_alive(&self, _run: bool, _eager: bool) -> Result<Service, String> {
        Ok(Service::default())
    }
    pub async fn listen(
        &self,
        _on_open: impl Fn(String) + Send + Sync + 'static,
        _on_disconnect: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), String> {
        Ok(())
    }
    pub async fn take_opened(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    pub async fn capture(&self, _rect: CaptureRect) -> Result<Capture, String> {
        Err("can't capture on this device".into())
    }
    pub async fn configure_pull(&self, _dir: &str, _enabled: bool) -> Result<(), String> {
        Ok(())
    }
    pub async fn wake_at(&self, _at: u64) -> Result<(), String> {
        Ok(())
    }
    pub async fn save_download(
        &self,
        _path: &str,
        _name: &str,
        _mime: &str,
    ) -> Result<Saved, String> {
        Err("not supported on this platform".into())
    }
    pub async fn open_download(&self, _uri: &str, _mime: &str) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
}

pub trait BackgroundExt<R: Runtime> {
    fn background(&self) -> &Background<R>;
}

impl<R: Runtime, T: Manager<R>> BackgroundExt<R> for T {
    fn background(&self) -> &Background<R> {
        self.state::<Background<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("tessera-background")
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            let background = Background {
                handle: _api
                    .register_android_plugin("dev.tessera.background", "BackgroundPlugin")?,
            };
            #[cfg(not(target_os = "android"))]
            let background = Background::<R> {
                _runtime: std::marker::PhantomData,
            };
            app.manage(background);
            Ok(())
        })
        .build()
}
