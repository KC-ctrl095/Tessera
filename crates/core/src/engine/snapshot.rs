//! Bound rendered snapshots, without deleting or rewriting durable history.
use super::*;
use std::io;

const STUB: &str = "This message is too large to show here; it is kept in full on the server.";

#[derive(Default)]
struct Size(usize);
impl io::Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn size(value: &impl serde::Serialize) -> Result<usize, ApiError> {
    let mut size = Size::default();
    serde_json::to_writer(&mut size, value).map_err(internal)?;
    Ok(size.0)
}

/// Check the transformed entry before it is queued or appended. Moving its
/// content through the entry avoids another copy, and maximal metadata widths
/// conservatively cover the sequence/timestamp assigned at append time.
pub(super) fn checked_private_content(
    content: Vec<ContentPart>,
) -> Result<Vec<ContentPart>, ApiError> {
    let entry = Entry {
        seq: u64::MAX,
        ts: u64::MAX,
        body: EntryBody::User {
            content,
            author: None,
        },
    };
    if size(&entry)? > MAX_SERIALIZED_RESPONSE_BYTES - RESPONSE_ENVELOPE_BYTES {
        return Err(bad(
            "These files are too large to include in a private chat message; send fewer or smaller files.",
        ));
    }
    let EntryBody::User { content, .. } = entry.body else {
        unreachable!()
    };
    Ok(content)
}

pub(super) fn bounded(snapshot: SessionSnapshot) -> Result<SessionSnapshot, ApiError> {
    // Leave 1 MiB for the ServerMessage envelope. One maximum admitted
    // content entry still fits; several cannot be bundled without a stub.
    bounded_at(snapshot, MAX_SERIALIZED_RESPONSE_BYTES - 1024 * 1024)
}

fn bounded_at(mut snapshot: SessionSnapshot, budget: usize) -> Result<SessionSnapshot, ApiError> {
    let mut total = size(&snapshot)?;
    if total <= budget {
        return Ok(snapshot);
    }
    // Bound transient/duplicate display state before touching durable entries,
    // so a delivering message copied in the queue cannot displace its history.
    for queued in &mut snapshot.queued {
        if total <= budget {
            break;
        }
        let old = size(queued)?;
        if old <= 4096 {
            continue;
        }
        queued.text = STUB.into();
        total = total.saturating_sub(old).saturating_add(size(queued)?);
    }
    if total > budget
        && let Some(streaming) = snapshot.streaming.take()
    {
        total = total.saturating_sub(size(&streaming)?);
    }
    // Oldest large bodies become explicit display stubs first, so the latest
    // entry remains complete. Identity/sequence/timestamps are preserved.
    for entry in &mut snapshot.entries {
        if total <= budget {
            break;
        }
        let old = size(entry)?;
        if old <= 4096 {
            continue;
        }
        entry.body = EntryBody::Notice {
            level: NoticeLevel::Info,
            text: STUB.into(),
            learned: vec![],
        };
        total = total.saturating_sub(old).saturating_add(size(entry)?);
    }
    if total > budget {
        // Extremely long histories may exceed the budget even as small
        // entries/stubs. Keep a tail, never mutate persisted history.
        let mut drop_prefix = 0;
        for entry in &snapshot.entries {
            if total <= budget {
                break;
            }
            total = total.saturating_sub(size(entry)? + 1);
            drop_prefix += 1;
        }
        snapshot.entries.drain(..drop_prefix);
    }
    // Unusually huge approval/session metadata gets a plain API error rather
    // than a message which disconnects the client. Stored history remains
    // intact even when a body cannot currently be fetched separately.
    if size(&snapshot)? > budget {
        return Err(bad("session metadata is too large to show here"));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn paged_snapshots_preserve_legacy_reads_and_exclusive_cursors() {
        let (engine, _dir, rt) = test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        for n in 0..305 {
            engine
                .0
                .append(
                    &rt,
                    EntryBody::User {
                        content: vec![ContentPart::text(format!("entry {n}"))],
                        author: None,
                    },
                )
                .unwrap();
        }
        let request: Request = serde_json::from_value(
            serde_json::json!({"method":"get_session_page","params":{"session_id":id}}),
        )
        .unwrap();
        let page: SessionSnapshot =
            serde_json::from_value(engine.handle(request, None).await.unwrap()).unwrap();
        assert_eq!(page.entries.first().unwrap().seq, 186);
        assert_eq!(page.entries.last().unwrap().seq, 305);
        assert_eq!(page.history_before, Some(186));
        let older: SessionSnapshot = serde_json::from_value(
            engine
                .handle(
                    Request::GetSessionPage {
                        session_id: id.clone(),
                        before_seq: page.history_before,
                        limit: Some(200),
                    },
                    None,
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(older.entries.first().unwrap().seq, 1);
        assert_eq!(older.entries.last().unwrap().seq, 185);
        assert_eq!(older.history_before, None);
        let full = engine.0.snapshot(&id, 0).unwrap();
        assert_eq!(full.entries.len(), 305);
        assert!(full.history_before.is_none());
        let incremental = engine.0.snapshot(&id, 300).unwrap();
        assert_eq!(incremental.entries.len(), 5);
        assert!(incremental.history_before.is_none());
        engine
            .0
            .append(
                &rt,
                EntryBody::User {
                    content: vec![ContentPart::text("[Completion check] fixture")],
                    author: Some("harness".into()),
                },
            )
            .unwrap();
        let hidden = engine
            .0
            .snapshot_with_page(&id, 0, Some((None, 1)))
            .unwrap();
        assert!(hidden.entries.is_empty());
        assert_eq!(hidden.history_before, Some(306));
        let older = engine
            .0
            .snapshot_with_page(&id, 0, Some((hidden.history_before, 1)))
            .unwrap();
        assert_eq!(older.entries[0].seq, 305);
        let mut legacy = serde_json::to_value(full).unwrap();
        legacy.as_object_mut().unwrap().remove("history_before");
        assert!(
            serde_json::from_value::<SessionSnapshot>(legacy)
                .unwrap()
                .history_before
                .is_none()
        );
    }
    #[tokio::test]
    async fn duplicate_queued_display_does_not_displace_the_latest_complete_entry() {
        let (engine, _dir, rt) = test_support::engine().await;
        for _ in 0..2 {
            engine
                .0
                .append(
                    &rt,
                    EntryBody::User {
                        content: vec![ContentPart::text("x".repeat(8192))],
                        author: None,
                    },
                )
                .unwrap();
        }
        let id = rt.st.lock().info.id.clone();
        let mut snapshot = engine.0.snapshot(&id, 0).unwrap();
        snapshot.queued.push(QueuedMessage {
            text: "x".repeat(8192),
            steer: false,
            interrupt: false,
            attachments: 0,
        });
        let before = snapshot.entries.len();
        let budget = size(&snapshot).unwrap() - 2 * 8192 + 1024;
        let bounded = bounded_at(snapshot, budget).unwrap();
        assert!(size(&bounded).unwrap() <= budget);
        assert_eq!(bounded.entries.len(), before);
        assert!(
            matches!(&bounded.entries[0].body, EntryBody::Notice { text, .. } if text.contains("too large to show here"))
        );
        assert!(
            matches!(&bounded.entries[1].body, EntryBody::User { content, .. } if matches!(&content[0], ContentPart::Text { text } if text.len() == 8192))
        );
        assert!(bounded.queued[0].text.contains("too large to show here"));
        assert!(
            engine
                .store()
                .entries(&id, 0)
                .unwrap()
                .iter()
                .all(|entry| matches!(entry.body, EntryBody::User { .. }))
        );
    }
}
