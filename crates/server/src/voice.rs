//! One connection owns its speech operations; cancel cannot reach another device.
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
use tessera_proto::{ApiError, ErrorCode, Request};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(super) struct Operations(Arc<Mutex<HashMap<String, CancellationToken>>>);
pub(super) struct Operation {
    owner: Operations,
    id: String,
    pub cancel: CancellationToken,
}
impl Operations {
    pub fn start(&self, request: &Request) -> Result<Option<Operation>, ApiError> {
        let id = match request {
            Request::Transcribe { operation, .. } | Request::Speak { operation, .. } => operation,
            _ => return Ok(None),
        };
        let Some(id) = id else {
            return Ok(None);
        };
        if id.is_empty() || id.len() > 128 {
            return Err(ApiError::new(
                ErrorCode::BadRequest,
                "Invalid voice operation id.",
            ));
        }
        let mut active = self.0.lock();
        if active.contains_key(id) {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "Voice operation id is already active.",
            ));
        }
        let cancel = CancellationToken::new();
        active.insert(id.clone(), cancel.clone());
        Ok(Some(Operation {
            owner: self.clone(),
            id: id.clone(),
            cancel,
        }))
    }
    pub fn active(&self) -> bool {
        !self.0.lock().is_empty()
    }

    pub fn cancel(&self, id: &str) {
        if let Some(cancel) = self.0.lock().get(id) {
            cancel.cancel();
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.owner.0.lock().remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_admission_and_cancel_are_scoped_until_guard_drop() {
        let first = Operations::default();
        let other = Operations::default();
        let request = Request::Speak {
            text: "test".into(),
            engine: None,
            operation: Some("fixture".into()),
        };
        let op = first.start(&request).unwrap().unwrap();
        assert!(first.active());
        assert!(first.start(&request).is_err());
        other.cancel("fixture");
        assert!(!op.cancel.is_cancelled());
        first.cancel("fixture");
        assert!(op.cancel.is_cancelled());
        assert!(first.active());
        drop(op);
        assert!(!first.active());
        assert!(first.start(&request).is_ok());
    }
}
