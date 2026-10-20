//! User picker chunks, on this server or a machine's browser.
use super::Tab;
use serde_json::json;
use tessera_proto::{BrowserUploadAction, BrowserUploadResult};

impl Tab {
    pub(crate) async fn upload_stream(
        &self,
        chooser: &str,
        viewer: u64,
        action: BrowserUploadAction,
    ) -> Result<BrowserUploadResult, String> {
        let browser = self.browser().ok_or("the browser stopped")?;
        let owner = format!("{viewer}:{}:{chooser}", self.key());
        let finishing = match &action {
            BrowserUploadAction::Finish { upload } => Some(upload.clone()),
            _ => None,
        };
        let rollback = finishing
            .as_ref()
            .filter(|_| browser.0.remote.is_none())
            .map(|token| {
                super::upload::Rollback::new(|| {
                    let _ = browser.0.uploads.lock().cancel(&owner, token);
                })
            });
        // Cancel belongs to the stored picker capability, not today's chooser.
        if let BrowserUploadAction::Cancel { ref upload } = action
            && browser.0.remote.is_none()
        {
            browser.0.uploads.lock().cancel(&owner, upload)?;
            return Ok(BrowserUploadResult {
                upload: None,
                received: 0,
            });
        }
        let cancelling = matches!(action, BrowserUploadAction::Cancel { .. });
        let (node, many) = {
            let st = self.st.lock();
            if cancelling || (finishing.is_some() && browser.0.remote.is_some()) {
                (
                    st.file_chooser
                        .as_ref()
                        .filter(|(id, _, _)| id == chooser)
                        .map(|(_, node, _)| *node)
                        .unwrap_or(0),
                    true,
                )
            } else {
                let (_, node, many) = st
                    .file_chooser
                    .as_ref()
                    .filter(|(id, _, _)| id == chooser)
                    .ok_or("that file chooser closed")?;
                (*node, *many)
            }
        };
        if let BrowserUploadAction::Start { ref files } = action
            && !many
            && files.len() > 1
        {
            return Err("choose one file for this page".into());
        }
        let link = self.link()?;
        if browser.0.remote.is_some() {
            let result = link
                .cdp
                .call(
                    Some(&link.session),
                    "Tessera.uploadFiles",
                    json!({"backendNodeId":node,"owner":owner,"action":action}),
                )
                .await?;
            if matches!(action, BrowserUploadAction::Finish { .. }) {
                {
                    let mut st = self.st.lock();
                    if st
                        .file_chooser
                        .as_ref()
                        .is_some_and(|(id, _, _)| id == chooser)
                    {
                        st.file_chooser = None;
                    }
                }
                self.publish();
                return Ok(BrowserUploadResult {
                    upload: None,
                    received: 0,
                });
            }
            return serde_json::from_value(result)
                .map_err(|_| "the upload could not finish".into());
        }
        if matches!(action, BrowserUploadAction::Start { .. }) {
            let weak = std::sync::Arc::downgrade(&browser.0);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3601)).await;
                if let Some(shared) = weak.upgrade() {
                    shared.uploads.lock().expire();
                }
            });
        }
        let mut paths = None;

        let result = {
            let mut u = browser.0.uploads.lock();
            match action {
                BrowserUploadAction::Start { files } => BrowserUploadResult {
                    upload: Some(u.start(&owner, &files)?),
                    received: 0,
                },
                BrowserUploadAction::Chunk {
                    upload,
                    index,
                    data,
                } => BrowserUploadResult {
                    upload: None,
                    received: u.chunk(&owner, &upload, index, &data)?,
                },
                BrowserUploadAction::Finish { upload } => {
                    paths = Some(u.finish(&owner, &upload)?);
                    BrowserUploadResult {
                        upload: None,
                        received: 0,
                    }
                }
                BrowserUploadAction::Cancel { upload } => {
                    u.cancel(&owner, &upload)?;
                    BrowserUploadResult {
                        upload: None,
                        received: 0,
                    }
                }
            }
        };
        if let Some(paths) = paths {
            if self.st.lock().file_chooser.as_ref().map(|c| c.0.as_str()) != Some(chooser) {
                return Err(
                    "The page changed before the selected files were sent. Choose again.".into(),
                );
            }
            self.link()?;
            link.cdp
                .call(
                    Some(&link.session),
                    "DOM.setFileInputFiles",
                    json!({"backendNodeId":node,"files":paths}),
                )
                .await?;
            browser
                .0
                .uploads
                .lock()
                .commit(&owner, finishing.as_ref().unwrap())?;
            rollback.unwrap().commit();
            {
                let mut st = self.st.lock();
                if st
                    .file_chooser
                    .as_ref()
                    .is_some_and(|(id, _, _)| id == chooser)
                {
                    st.file_chooser = None;
                }
            }
            self.publish();
        }
        Ok(result)
    }
}
