//! Bounded display pages and the model-visible region of very long histories.
use super::*;

/// Display entries in ascending order, with an exclusive cursor for older rows.
pub struct HistoryPage {
    pub entries: Vec<Entry>,
    pub before: Option<u64>,
    pub revision: u64,
}

const PAGE_BYTES: usize = 512 * 1024;

impl Store {
    /// Read only the requested tail. Large individual entries stay whole, and
    /// at least one row is consumed so an exclusive cursor always advances.
    pub fn entries_page(&self, id: &str, before: Option<u64>, limit: u32) -> Result<HistoryPage> {
        store_call!(self.entries_page(id, before, limit));
        let limit = limit.clamp(1, 200) as usize;
        if let Some(p) = self.private.lock().get(id) {
            let end = p
                .entries
                .partition_point(|e| before.is_none_or(|before| e.seq < before));
            let mut start = end;
            let mut bytes = 0;
            while start > 0 && end - start < limit {
                let size = serde_json::to_vec(&p.entries[start - 1].body)?.len();
                if start < end && bytes + size > PAGE_BYTES {
                    break;
                }
                bytes += size;
                start -= 1;
            }
            return Ok(HistoryPage {
                entries: p.entries[start..end].to_vec(),
                before: (start > 0).then(|| p.entries[start].seq),
                revision: p.history_revision,
            });
        }
        let conn = self.conn.lock();
        // The revision and rows must describe one database view, including
        // when another process rewinds this chat while a page is being read.
        let tx = conn.unchecked_transaction()?;
        let revision = tx
            .query_row(
                "SELECT revision FROM session_history_revisions WHERE session_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut stmt = tx.prepare_cached(
            "SELECT seq, ts, body FROM entries WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let mut rows = stmt.query(params![
            id,
            before.unwrap_or(i64::MAX as u64).min(i64::MAX as u64) as i64,
            limit + 1
        ])?;
        let mut entries: Vec<Entry> = Vec::new();
        let mut bytes = 0;
        let mut more = false;
        while let Some(row) = rows.next()? {
            if entries.len() == limit {
                more = true;
                break;
            }
            let body = row.get_ref(2)?.as_str()?;
            if !entries.is_empty() && bytes + body.len() > PAGE_BYTES {
                more = true;
                break;
            }
            bytes += body.len();
            entries.push(Entry {
                seq: row.get::<_, i64>(0)? as u64,
                ts: row.get::<_, i64>(1)? as u64,
                body: serde_json::from_str(body)?,
            });
        }
        let before = more.then(|| entries.last().expect("a page advances").seq);
        entries.reverse();
        Ok(HistoryPage {
            entries,
            before,
            revision,
        })
    }

    /// Model preparation and compaction need the latest checkpoint and its
    /// verbatim region, not the raw history that checkpoint already folded.
    /// Keep this smaller region in the existing cache, under the same budget.
    /// Recall, forks and display APIs still read the complete durable history.
    pub fn context_history(&self, id: &str) -> Result<std::sync::Arc<Vec<Entry>>> {
        store_call!(self.context_history(id));
        if let Some(p) = self.private.lock().get(id) {
            let from = crate::context::last_compaction(&p.entries)
                .map_or(0, |(seq, _, first)| first.min(seq));
            let start = p.entries.partition_point(|e| e.seq < from);
            return Ok(std::sync::Arc::new(p.entries[start..].to_vec()));
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let version: i64 = tx
            .prepare_cached("PRAGMA data_version")?
            .query_row([], |r| r.get(0))?;
        let mut cache = self.histories.lock();
        cache.check(version);
        if let Some(all) = cache.get_context(id) {
            return Ok(all);
        }
        drop(cache);
        let checkpoint: Option<(u64, String)> = tx.query_row(
            "SELECT seq, body FROM entries WHERE session_id = ?1 AND kind = 'compaction' AND COALESCE(json_extract(body, '$.external'), 0) = 0 ORDER BY seq DESC LIMIT 1",
            [id], |r|Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let from = match checkpoint {
            Some((seq, body)) => match serde_json::from_str::<EntryBody>(&body)? {
                EntryBody::Compaction { first_kept_seq, .. } => first_kept_seq.min(seq),
                _ => 0,
            },
            None => 0,
        };
        let (entries, bytes) = read_entries(&tx, id, from.saturating_sub(1))?;
        let all = std::sync::Arc::new(entries);
        self.histories
            .lock()
            .insert_context(id, all.clone(), bytes, from > 1);
        Ok(all)
    }
}
