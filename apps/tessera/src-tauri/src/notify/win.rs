//! WinRT foreground activations; no COM server, auto-start or cold-start
//! launch command is registered. A toast click only uses its stored chat.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tauri::{AppHandle, Manager};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::core::HSTRING;

const GROUP: &str = "tessera-chats";

struct Apartment(bool);
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        // SAFETY: balance a successful initialization on this same thread.
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self(true)),
            Err(e) if e.code() == windows::Win32::Foundation::RPC_E_CHANGED_MODE => Ok(Self(false)),
            Err(e) => Err(e),
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::WinRT::RoUninitialize() };
        }
    }
}

struct Live {
    toast: ToastNotification,
    token: i64,
    active: Arc<AtomicBool>,
}
impl Drop for Live {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let _ = self.toast.RemoveActivated(self.token);
    }
}

#[derive(Default)]
pub(super) struct Toasts(Mutex<HashMap<String, Live>>);

impl Toasts {
    pub fn show(&self, app: &AppHandle, note: &super::Note) -> windows::core::Result<bool> {
        let _apartment = Apartment::new()?;
        let notifications = app.state::<super::Notifier>();
        let state = app.state::<crate::AppState>();
        let conn = state.conn.lock();
        let inner = notifications.inner.lock();
        let Some(payload) = inner.platform_payload(note) else {
            return Ok(false);
        };
        let Some((_, client)) = conn.as_ref().cloned() else {
            return Ok(false);
        };

        self.show_note(
            app,
            &payload,
            Some((
                note.generation,
                note.transport_epoch,
                note.client_epoch,
                client,
            )),
        )?;
        Ok(true)
    }

    /// Deliberately separate from connection posts: fixed local text, no click
    /// target, valid even before signing in. Never upgrades an unstamped post.
    pub fn show_sample(&self, app: &AppHandle) -> windows::core::Result<()> {
        let _apartment = Apartment::new()?;
        self.show_note(app, &super::Note::sample(), None)
    }

    fn show_note(
        &self,
        app: &AppHandle,
        note: &super::Note,
        activation: Option<(u64, u64, Option<u64>, tessera_client::Client)>,
    ) -> windows::core::Result<()> {
        self.close(app, &note.key)?;
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(super::toast_content::xml(
            &note.title,
            &note.body,
            note.silent,
        )))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(super::toast_content::tag(&note.key)))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        toast.SetSuppressPopup(note.silent)?;
        let active = Arc::new(AtomicBool::new(true));
        let live = active.clone();
        let app2 = app.clone();
        let session = note.session.clone();
        let token = toast.Activated(&TypedEventHandler::new(move |_, _| {
            if live.swap(false, Ordering::AcqRel)
                && let (Some(session), Some((generation, epoch, Some(client_epoch), client))) =
                    (&session, &activation)
            {
                super::opened_on(
                    &app2,
                    session.clone(),
                    *generation,
                    *epoch,
                    *client_epoch,
                    client,
                );
            }
            Ok(())
        }))?;
        let entry = Live {
            toast,
            token,
            active,
        };
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            app.config().identifier.as_str(),
        ))?;
        notifier.Show(&entry.toast)?;
        let mut entries = self.0.lock();
        // Bound event handlers retained for chats not opened again. Removing
        // the handler makes an old toast inert; history can still be dismissed.
        if entries.len() >= 512 {
            entries.clear();
        }
        entries.insert(note.key.clone(), entry);
        Ok(())
    }

    pub fn close(&self, app: &AppHandle, key: &str) -> windows::core::Result<()> {
        let _apartment = Apartment::new()?;
        if let Some(entry) = self.0.lock().remove(key) {
            let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
                app.config().identifier.as_str(),
            ))?;
            notifier.Hide(&entry.toast)?;
        }
        ToastNotificationManager::History()?.RemoveGroupedTagWithId(
            &HSTRING::from(super::toast_content::tag(key)),
            &HSTRING::from(GROUP),
            &HSTRING::from(app.config().identifier.as_str()),
        )
    }
}
