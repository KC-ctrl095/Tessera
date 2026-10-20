//! Private chats stay on the device that made them (batch 17). They were
//! always held only in this server's memory; now they are also that one
//! device's alone: no other device, web browser or terminal app lists them,
//! opens them, gets their events or notifications, or reaches what belongs to
//! them (their web views, files, approvals). Enforced here, on the server,
//! for every request and every event, whatever an app shows.
//!
//! A device is its login (`devices` row): the same device reconnecting is
//! still the same; a device signed out takes its private chats with it.
//! This server itself (`device: None`: tests, `tessera run`) sees everything.

use super::*;

impl Inner {
    /// Whether `device` may see session `id`. Only a private chat is any one
    /// device's (a session started under one is private too, and its
    /// device's: see `create_session_ext`); `None` is this server itself.
    pub(super) fn device_sees(&self, id: &str, device: Option<&str>) -> bool {
        let Some(device) = device else { return true };
        match self.store.private_owner(id) {
            None => true,
            Some(owner) => owner.as_deref() == Some(device),
        }
    }

    /// A board's chat may be seen by `device` (a board that isn't there: yes,
    /// the request fails on its own).
    fn device_sees_board(&self, board: &str, device: Option<&str>) -> bool {
        device.is_none()
            || match self.store.board(board) {
                Ok(Some(b)) => self.device_sees(&b.session_id, device),
                _ => true,
            }
    }

    /// The sessions, boards and files a request names must all be ones
    /// `device` may see; otherwise it fails as if they didn't exist.
    pub(super) fn check_private(
        &self,
        req: &Request,
        device: Option<&str>,
    ) -> Result<(), ApiError> {
        if device.is_none() {
            return Ok(());
        }
        let hidden = |what: &str| ApiError::new(ErrorCode::NotFound, format!("no {what}"));
        let session = |id: &str| -> Result<(), ApiError> {
            if self.device_sees(id, device) {
                Ok(())
            } else {
                Err(hidden(&format!("session `{id}`")))
            }
        };
        let board = |id: &str| -> Result<(), ApiError> {
            if self.device_sees_board(id, device) {
                Ok(())
            } else {
                Err(hidden(&format!("whiteboard `{id}`")))
            }
        };
        match req {
            Request::GetSession { session_id, .. }
            | Request::GetSessionPage { session_id, .. }
            | Request::UpdateSession { session_id, .. }
            | Request::SetSessionDaybreak { session_id, .. }
            | Request::SetChatHelpers { session_id, .. }
            | Request::DeleteSession { session_id }
            | Request::SetGoal { session_id, .. }
            | Request::Dequeue { session_id, .. }
            | Request::EditQueued { session_id, .. }
            | Request::SteerQueued { session_id, .. }
            | Request::InterruptQueued { session_id, .. }
            | Request::ForkSession { session_id, .. }
            | Request::Rewind { session_id, .. }
            | Request::Send { session_id, .. }
            | Request::Abort { session_id }
            | Request::Compact { session_id, .. }
            | Request::DecideChatSecret { session_id, .. }
            | Request::BrowserWatch { session_id, .. }
            | Request::CreateBoard { session_id, .. } => session(session_id),
            Request::RespondApproval {
                session_id,
                request_id,
                ..
            } => {
                session(session_id)?;
                // The approval itself, whatever chat the request names.
                let shown_on = self
                    .approvals
                    .lock()
                    .get(request_id)
                    .map(|p| p.shown_on.clone());
                match shown_on {
                    Some(s) => session(&s),
                    None => Ok(()),
                }
            }
            Request::ListBoards {
                session_id: Some(s),
            }
            | Request::SaveNote {
                session_id: Some(s),
                ..
            }
            | Request::SendNotification {
                session_id: Some(s),
                ..
            }
            | Request::SeenNotifications {
                session_id: Some(s),
                ..
            } => session(s),
            Request::ListSharedFiles {
                session_id,
                board_id,
            } => {
                if let Some(s) = session_id {
                    session(s)?;
                }
                match board_id {
                    Some(b) => board(b),
                    None => Ok(()),
                }
            }
            Request::GetBoard { id }
            | Request::DeleteBoard { id }
            | Request::AnswerBoard { id, .. }
            | Request::BoardFetch { id, .. }
            | Request::BoardLink { id } => board(id),
            Request::BrowserShowOnScreen { board_id, .. }
            | Request::BrowserAttach { board_id, .. }
            | Request::BrowserDetach { board_id }
            | Request::BrowserInput { board_id, .. }
            | Request::BrowserNav { board_id, .. }
            | Request::BrowserPage { board_id, .. }
            | Request::BrowserNewTab { board_id, .. }
            | Request::BrowserUpload { board_id, .. }
            | Request::BrowserAck { board_id, .. }
            | Request::BrowserDialogAnswer { board_id, .. }
            | Request::BrowserChooseFiles { board_id, .. }
            | Request::BrowserToServer { board_id } => board(board_id),
            Request::ReadSharedFile { id, .. } => {
                match self.store.shared_file(id) {
                    Ok(Some((f, _))) => {
                        let ok = f
                            .session_id
                            .as_deref()
                            .is_none_or(|s| self.device_sees(s, device))
                            && f.board_id
                                .as_deref()
                                .is_none_or(|b| self.device_sees_board(b, device));
                        if ok {
                            Ok(())
                        } else {
                            // As the server says of a file that is gone.
                            Err(ApiError::new(
                                ErrorCode::NotFound,
                                "this file is no longer on the server",
                            ))
                        }
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// Whether an event for every device may go to `device`: nothing about
    /// another device's private chat (its updates, stream, approvals,
    /// notices, notifications, web views, files).
    pub(super) fn event_for_device(&self, event: &Event, device: &str) -> bool {
        let device = Some(device);
        match event {
            // A chat that is gone (a private one deleted while this waited
            // in the queue) fails closed.
            Event::Notification { notification } => {
                self.notification_delivery(notification, device)
            }
            Event::BoardSnapshotRequest { board_id, .. } => {
                self.device_sees_board(board_id, device)
            }
            Event::BrowserState { tab } => self.device_sees_board(&tab.board_id, device),
            e => e.session_id().is_none_or(|s| self.device_sees(s, device)),
        }
    }

    /// A device was signed out: its private chats go at once (wiped from
    /// memory), as nobody else may ever open them.
    pub(super) fn private_signed_out(&self, device: &str) {
        for id in self.store.private_of(device) {
            if let Err(e) = self.remove_session(id) {
                tracing::warn!("removing a signed-out device's private chat: {}", e.message);
            }
        }
    }
}

impl Engine {
    /// See [`Inner::event_for_device`]: the server's connection loop asks
    /// before it sends a device an event meant for every device.
    pub fn event_for_device(&self, event: &Event, device: &str) -> bool {
        self.0.event_for_device(event, device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;

    async fn engine() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n",
        )
        .unwrap();
        let e = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        (e, dir)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        e: &Engine,
        req: Request,
        device: Option<&str>,
    ) -> Result<T, ApiError> {
        e.handle(req, device)
            .await
            .map(|v| serde_json::from_value(v).unwrap())
    }

    #[tokio::test]
    async fn deleted_private_events_keep_their_audience() {
        let (e, _dir) = engine().await;
        for signed_out in [false, true] {
            let mine: SessionInfo = call(
                &e,
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: None,
                    bot_id: None,
                    private: true,
                    lean: false,
                },
                Some("phone"),
            )
            .await
            .unwrap();
            let buffered = [
                Event::SessionUpdated {
                    session: mine.clone(),
                },
                Event::StreamEnded {
                    session_id: mine.id.clone(),
                },
                Event::SessionRemoved {
                    session_id: mine.id.clone(),
                },
            ];
            if signed_out {
                e.0.private_signed_out("phone");
            } else {
                e.0.remove_session(mine.id.clone()).unwrap();
            }
            assert!(e.0.store.get_session(&mine.id).unwrap().is_none());
            assert!(e.0.store.entries(&mine.id, 0).unwrap().is_empty());
            for event in buffered {
                assert!(e.event_for_device(&event, "phone"));
                assert!(!e.event_for_device(&event, "laptop"));
            }
            assert!(
                e.0.store
                    .append(
                        &mine.id,
                        EntryBody::User {
                            content: vec![ContentPart::text("late")],
                            author: None,
                        }
                    )
                    .is_err()
            );
        }
    }

    /// The device that made a private chat sees it and what belongs to it;
    /// another device doesn't, as if it didn't exist; the server itself does.
    #[tokio::test]
    async fn a_private_chat_is_its_devices_alone() {
        let (e, _dir) = engine().await;
        let mine: SessionInfo = call(
            &e,
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: None,
                private: true,
                lean: false,
            },
            Some("phone"),
        )
        .await
        .unwrap();
        let ids = |l: Vec<SessionInfo>| l.into_iter().map(|s| s.id).collect::<Vec<_>>();
        let list = |d: Option<&'static str>| {
            let e = e.clone();
            async move {
                ids(call(&e, Request::ListSessions { archived: false }, d)
                    .await
                    .unwrap())
            }
        };
        assert!(list(Some("phone")).await.contains(&mine.id));
        assert!(!list(Some("laptop")).await.contains(&mine.id));
        assert!(list(None).await.contains(&mine.id), "the server itself");
        // Whiteboards (web views) aren't made in private chats at all.
        let refused = call::<Whiteboard>(
            &e,
            Request::CreateBoard {
                session_id: mine.id.clone(),
                title: "Page".into(),
                content: BoardContent::Markdown {
                    markdown: "hi".into(),
                },
                placed: false,
            },
            Some("phone"),
        )
        .await;
        assert!(refused.is_err());
        // Another device doesn't even find it to try.
        let hidden = call::<Value>(
            &e,
            Request::GetSession {
                session_id: mine.id.clone(),
                since_seq: None,
            },
            Some("laptop"),
        )
        .await
        .unwrap_err();
        assert_eq!(hidden.code, ErrorCode::NotFound);
        assert!(hidden.message.contains("no session"), "{}", hidden.message);
        // Events: the phone's only.
        let ev = Event::SessionUpdated {
            session: mine.clone(),
        };
        assert!(e.event_for_device(&ev, "phone") && !e.event_for_device(&ev, "laptop"));
        let ev = Event::StreamEnded {
            session_id: mine.id.clone(),
        };
        assert!(!e.event_for_device(&ev, "laptop"));
        let n = Event::Notification {
            notification: UserNotification {
                id: 1,
                title: "t".into(),
                body: "b".into(),
                session_id: Some(mine.id.clone()),
                from: "Private chat".into(),
                urgent: false,
                created_at: 0,
                due_at: 0,
                sent: true,
                seen: false,
                kind: String::new(),
                quiet: false,
                private: true,
                source_key: String::new(),
            },
        };
        assert!(e.event_for_device(&n, "phone") && !e.event_for_device(&n, "laptop"));
        // Signing the phone out takes it along.
        e.0.private_signed_out("phone");
        assert!(!list(None).await.contains(&mine.id));
    }
}
