//! Rewind: put back the files the agent changed since a point in the
//! conversation, and/or cut the conversation back to that point (the full
//! history is kept as a new "(rewound)" session, so nothing is lost).
//!
//! Files come back from the snapshots `write` and `edit` take before every
//! change (and `agents apply` before applying a subagent's patch), including
//! those of the session's subagents; in Claude Code chats from those its
//! `PreToolUse` hook takes before its Edit, Write and NotebookEdit change a
//! file (its subagents' too, `engine/claude_code.rs`). Changes made by shell
//! commands are not tracked, so they are not undone; the result lists those
//! commands.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::store::FileSnapshot;

impl Inner {
    pub(super) fn rewind(
        self: &Arc<Self>,
        id: &str,
        to_seq: u64,
        files: bool,
        conversation: bool,
        dry_run: bool,
    ) -> Result<RewindResult, ApiError> {
        if !files && !conversation {
            return Err(bad(
                "choose what to rewind: `files`, `conversation` or both",
            ));
        }
        if self.store.is_private(id) {
            return Err(bad("private chats can't be rewound"));
        }
        let rt = self.rt(id)?;
        let (info, running) = {
            let mut st = rt.st.lock();
            // Held for the whole rewind, decided with the running check:
            // nothing Claude Code starts by itself goes on meanwhile.
            if !dry_run && !st.running {
                st.rewinding = true;
            }
            (st.info.clone(), st.running)
        };
        let _held = Rewinding((!dry_run && !running).then_some(&*rt));
        if info.kind == SessionKind::Subagent {
            return Err(bad(
                "a subagent's session can't be rewound; rewind the chat that started it",
            ));
        }
        let children_running = self
            .background
            .lock()
            .get(id)
            .is_some_and(|l| l.iter().any(|a| !a.done));
        if running || children_running {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "the session is running; stop it (and its subagents) before rewinding",
            ));
        }
        if !dry_run {
            // What it kept for the next turn holds what may be cut now.
            self.claude.close_for(id);
        }
        let entries = self.store.entries(id, 0).map_err(internal)?;
        let to_seq = safe_point(&entries, to_seq);
        let after: Vec<&Entry> = entries.iter().filter(|e| e.seq > to_seq).collect();
        let calls: std::collections::HashSet<&str> = after
            .iter()
            .flat_map(|e| match &e.body {
                EntryBody::Assistant { content, .. } => content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::ToolCall { id, .. } => Some(id.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        let mut child_history = Vec::new();
        for child in self.store.children_of(id).map_err(internal)? {
            if child.kind == SessionKind::Subagent
                && child
                    .parent_call
                    .as_deref()
                    .is_some_and(|c| calls.contains(c))
            {
                child_history.push((
                    PathBuf::from(child.cwd),
                    self.store.entries(&child.id, 0).map_err(internal)?,
                ));
            }
        }
        let mut res = RewindResult {
            to_seq,
            dry_run,
            restored: Vec::new(),
            deleted: Vec::new(),
            skipped: Vec::new(),
            not_undone: shell_commands(&after),
            notes: Vec::new(),
            removed_entries: if conversation { after.len() as u64 } else { 0 },
            session: info.clone(),
            branch: None,
        };
        let Some(cutoff) = after.first().map(|e| e.ts) else {
            return Ok(res); // nothing after that point
        };
        let snaps = self.rewind_snapshots(id, to_seq, cutoff)?;
        let cwd = Path::new(&info.cwd);
        let shown = |p: &str| {
            Path::new(p)
                .strip_prefix(cwd)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.to_string())
        };
        for (_, entries) in &child_history {
            res.not_undone.extend(
                shell_commands(&entries.iter().collect::<Vec<_>>())
                    .into_iter()
                    .map(|cmd| format!("Helper: {cmd}")),
            );
        }
        if files {
            // Missing CLI snapshots must not look like a successful restore.
            let histories = std::iter::once((cwd, after.clone())).chain(
                child_history
                    .iter()
                    .map(|(dir, es)| (dir.as_path(), es.iter().collect())),
            );
            for (dir, entries) in histories {
                for e in &entries {
                    if let EntryBody::Assistant { content, .. } = &e.body {
                        for p in content {
                            if let AssistantPart::ToolCall {
                                name, arguments, ..
                            } = p
                                && crate::claude_code::is_edit_tool(name)
                                && let Some(path) = crate::claude_code::edit_path(arguments)
                            {
                                let path = dir.join(path).to_string_lossy().into_owned();
                                if !snaps.contains_key(&path)
                                    && !res.skipped.iter().any(|s| s.path == shown(&path))
                                {
                                    res.skipped.push(RewindSkip { path: shown(&path), reason: "No saved snapshot (unreadable, too large, not permitted or edit not observed). Left as it is.".into() });
                                }
                            }
                        }
                    }
                }
            }
            self.restore_files(&snaps, dry_run, &shown, &mut res);
        }
        if dry_run {
            return Ok(res);
        }
        if conversation {
            let branch = self.fork_as(id, None, format!("{} (rewound)", info.title))?;
            // Claude Code's process kept from the last turn holds what is cut now.
            self.claude.close_for(id);
            self.store
                .delete_snapshots_after(id, to_seq)
                .map_err(internal)?;
            {
                let mut st = rt.st.lock();
                self.store
                    .truncate_entries_revision(id, to_seq)
                    .map_err(internal)?;
                let kept = self.store.entries(id, 0).map_err(internal)?;
                st.info.last_seq = to_seq;
                st.info.updated_at = now_ms();
                st.info.preview = kept
                    .iter()
                    .rev()
                    .filter(|e| !crate::tools::meta::is_protocol_entry(e))
                    .find_map(|e| match &e.body {
                        EntryBody::User { content, .. } => super::user_preview(content),
                        EntryBody::Assistant { content, .. } => {
                            content.iter().rev().find_map(|p| match p {
                                AssistantPart::Text { text } if !text.trim().is_empty() => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        }
                        _ => None,
                    })
                    .map(|p| one_line(&p, 140))
                    .unwrap_or_default();
                // Context bookkeeping keyed by seq must not reach the entries
                // that will reuse the seqs just freed.
                let limit = to_seq + 1;
                st.internal.pruned_until = st.internal.pruned_until.min(limit);
                st.internal.demote_before = st.internal.demote_before.min(limit);
                st.internal.thinking_before = st.internal.thinking_before.min(limit);
                // A checkpoint written ahead of time may have read what is gone.
                st.internal.completion_acknowledged = None;
                st.internal.completion_pending = None;
                st.pre = None;
                // An environment note after the cut may have been removed.
                // Re-emit current facts rather than trust the old shown stamp.
                st.internal.environment = None;
                super::compaction::history_edited(&mut st);
            }
            self.save(&rt);
            let _ = self.events.send(Event::SessionRewound {
                session_id: id.to_string(),
                to_seq,
            });
            res.branch = Some(branch);
        }
        // Tell the agent what no longer matches its history.
        let paths: Vec<String> = {
            let mut v: Vec<String> = snaps.keys().map(|p| shown(p)).collect();
            v.dedup();
            v
        };
        let note = match (files, conversation) {
            (true, false) if !res.restored.is_empty() || !res.deleted.is_empty() => {
                let mut n = format!(
                    "[Rewind] The user put files back as they were after entry {to_seq} of this conversation; the edits made since are undone."
                );
                if !res.restored.is_empty() {
                    n.push_str(&format!(" Restored: {}.", res.restored.join(", ")));
                }
                if !res.deleted.is_empty() {
                    n.push_str(&format!(" Deleted: {}.", res.deleted.join(", ")));
                }
                Some(n)
            }
            (false, true) if !paths.is_empty() => Some(format!(
                "[Rewind] The user rewound the conversation to this point, but the files changed after it were kept as they are now: {}.",
                paths.join(", ")
            )),
            _ => None,
        };
        if let Some(mut n) = note {
            if !res.skipped.is_empty() {
                let left: Vec<String> = res.skipped.iter().map(|s| s.path.clone()).collect();
                n.push_str(&format!(" Left as they are: {}.", left.join(", ")));
            }
            if !res.not_undone.is_empty() {
                n.push_str(" Changes made by shell commands were not undone.");
            }
            let _ = self.append(
                &rt,
                EntryBody::User {
                    content: vec![ContentPart::text(n)],
                    author: Some("harness".into()),
                },
            );
        }
        res.session = rt.st.lock().info.clone();
        Ok(res)
    }

    /// Per path, the first and last snapshot taken after `to_seq`, by the
    /// session itself or by its subagents (those that edited this checkout
    /// directly: an isolated one's changes arrive through `agents apply`,
    /// which the session records itself).
    fn rewind_snapshots(
        &self,
        id: &str,
        to_seq: u64,
        cutoff: u64,
    ) -> Result<BTreeMap<String, (FileSnapshot, FileSnapshot)>, ApiError> {
        let children: Vec<(String, bool)> = self
            .store
            .list_sessions(false)
            .map_err(internal)?
            .into_iter()
            .filter(|s| s.kind == SessionKind::Subagent && s.parent_id.as_deref() == Some(id))
            .filter(|s| {
                !matches!(self.store.get_session(&s.id), Ok(Some((_, int)))
                    if int.subagent.as_ref().is_some_and(|sub| sub.worktree.is_some()))
            })
            .map(|s| (s.id, s.created_at >= cutoff))
            .collect();
        let snaps = self
            .store
            .snapshots_after(id, to_seq, &children, cutoff)
            .map_err(internal)?;
        let mut by_path: BTreeMap<String, (FileSnapshot, FileSnapshot)> = BTreeMap::new();
        for s in snaps {
            match by_path.get_mut(&s.path) {
                Some((_, last)) => *last = s,
                None => {
                    by_path.insert(s.path.clone(), (s.clone(), s));
                }
            }
        }
        Ok(by_path)
    }

    /// Post-edit hashes are reads too: the store only persists a hash already
    /// computed from an authorized object, never follows snapshot pathnames.
    pub(super) fn seal_snapshots(self: &Arc<Self>, sid: &str, seq: u64) -> Result<(), ApiError> {
        let rt = self.rt(sid)?;
        let cwd = rt.st.lock().info.cwd.clone();
        let ctx = ToolCtx {
            session_id: sid.into(),
            call_id: String::new(),
            admission: crate::tools::admission::ActionAdmission::default(),
            seq,
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(self.clone(), false)),
            progress: Arc::new(|_| {}),
            depth: 0,
            cleared: None,
        };
        for (id, path) in self.store.open_snapshots(sid, seq).map_err(internal)? {
            if let Ok(bytes) = rewind_current(Path::new(&path), &ctx) {
                let hash = bytes
                    .as_deref()
                    .map(crate::util::sha256_hex)
                    .unwrap_or_else(|| "-".into());
                self.store.seal_snapshot_hash(id, &hash).map_err(internal)?;
            }
        }
        Ok(())
    }
    fn restore_files(
        self: &Arc<Self>,
        snaps: &BTreeMap<String, (FileSnapshot, FileSnapshot)>,
        dry_run: bool,
        shown: &dyn Fn(&str) -> String,
        res: &mut RewindResult,
    ) {
        let ctx = ToolCtx {
            session_id: res.session.id.clone(),
            call_id: String::new(),
            admission: crate::tools::admission::ActionAdmission::default(),
            seq: 0,
            cwd: PathBuf::from(&res.session.cwd),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(self.clone(), false)),
            progress: Arc::new(|_| {}),
            depth: 0,
            cleared: None,
        };
        for (path, (first, last)) in snaps {
            let p = Path::new(path);
            let name = shown(path);
            let skip = |res: &mut RewindResult, reason: String| {
                res.skipped.push(RewindSkip {
                    path: name.clone(),
                    reason,
                })
            };
            if !p.is_absolute() {
                skip(res, "outside what the agent may change".into());
                continue;
            }
            let current = match rewind_current(p, &ctx) {
                Ok(bytes) => bytes,
                Err(e) => {
                    skip(res, e);
                    continue;
                }
            };
            let current_hash = current.as_deref().map(crate::util::sha256_hex);
            match &first.content {
                Some(before) => {
                    if current.as_deref() == Some(before.as_slice()) {
                        continue; // already as it was
                    }
                    if let (Some(after), Some(now)) = (&last.after_hash, &current_hash)
                        && after != now
                    {
                        res.notes.push(format!(
                            "{name} had changes made after the agent's last edit (by a command or by hand); {} them.",
                            if dry_run { "rewinding would overwrite" } else { "rewinding overwrote" }
                        ));
                    }
                    if !dry_run {
                        let written = crate::tools::file_object::Destination::prepare_guarded(
                            p,
                            |p| crate::tools::files::authorize_file(p, &ctx, false),
                            ctx.admission.clone(),
                        )
                        .and_then(|d| {
                            d.write_bytes(current.as_deref(), before)
                                .map_err(|e| e.to_string())
                        });
                        if let Err(e) = written {
                            skip(res, format!("could not write it: {e}"));
                            continue;
                        }
                    }
                    res.restored.push(name.clone());
                }
                // Created after that point: delete it, unless someone changed
                // it since the agent wrote it.
                None => {
                    let Some(now) = current_hash else {
                        continue; // already gone
                    };
                    match &last.after_hash {
                        Some(after) if *after == now => {
                            if !dry_run && let Err(e) = crate::tools::file_object::Destination::prepare_guarded(p,
                                |p| crate::tools::files::authorize_file(p, &ctx, false), ctx.admission.clone())
                                .and_then(|d| d.delete(current.as_deref().unwrap()).map_err(|e| e.to_string())) {
                                skip(res, format!("could not delete it: {e}"));
                                continue;
                            }
                            res.deleted.push(name.clone());
                        }
                        Some(_) => skip(
                            res,
                            "created by the agent, but changed since it wrote it; delete it yourself if it should go"
                                .into(),
                        ),
                        None => skip(
                            res,
                            "created by the agent, but whether it changed since can't be checked; delete it yourself if it should go"
                                .into(),
                        ),
                    }
                }
            }
        }
    }
}

pub(super) fn rewind_current(path: &Path, ctx: &ToolCtx) -> Result<Option<Vec<u8>>, String> {
    crate::tools::files::authorize_file(&crate::tools::canonical(path), ctx, false)?;
    match crate::tools::file_object::Opened::open(path) {
        Ok(opened) => {
            crate::tools::files::authorized_opened_bytes(&opened, ctx, 16 * 1024 * 1024).map(Some)
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path).is_err() =>
        {
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `to_seq`, moved back to before an assistant message whose tool calls
/// would otherwise lose their results.
fn safe_point(entries: &[Entry], to_seq: u64) -> u64 {
    let Some(asst) = entries.iter().rev().find(|e| {
        e.seq <= to_seq
            && matches!(&e.body, EntryBody::Assistant { content, .. }
                if content.iter().any(|p| matches!(p, AssistantPart::ToolCall { .. })))
    }) else {
        return to_seq;
    };
    let EntryBody::Assistant { content, .. } = &asst.body else {
        return to_seq;
    };
    let calls: Vec<&str> = content
        .iter()
        .filter_map(|p| match p {
            AssistantPart::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let split = entries.iter().any(|e| {
        e.seq > to_seq
            && matches!(&e.body, EntryBody::ToolResult { call_id, .. } if calls.contains(&call_id.as_str()))
    });
    if split {
        asst.seq.saturating_sub(1)
    } else {
        to_seq
    }
}

/// Shell commands in these entries (their effects can't be rewound).
fn shell_commands(entries: &[&Entry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => content
                .iter()
                .filter_map(|p| match p {
                    // Tessera's command tool, and Claude Code's.
                    AssistantPart::ToolCall {
                        name, arguments, ..
                    } if matches!(name.as_str(), "bash" | "Bash" | "PowerShell") => arguments
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|c| one_line(c, 200)),
                    AssistantPart::ToolCall {
                        name, arguments, ..
                    } if name == "machines"
                        && arguments
                            .get("action")
                            .and_then(Value::as_str)
                            .is_some_and(|a| matches!(a, "write" | "edit" | "run")) =>
                    {
                        Some(format!(
                            "Remote machine {}: {} (not restored)",
                            arguments
                                .get("machine")
                                .and_then(Value::as_str)
                                .unwrap_or("default"),
                            one_line(
                                arguments
                                    .get("command")
                                    .or_else(|| arguments.get("path"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("remote action"),
                                160
                            )
                        ))
                    }
                    AssistantPart::ToolCall { name, .. } if name.starts_with("mcp__") => {
                        Some(format!("{name}: external effects are not restored"))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

/// A chat being rewound (`RtState::rewinding`, set with its running check),
/// until dropped.
struct Rewinding<'a>(Option<&'a Rt>);

impl Drop for Rewinding<'_> {
    fn drop(&mut self) {
        if let Some(rt) = self.0 {
            rt.st.lock().rewinding = false;
        }
    }
}
#[cfg(test)]
mod revision_tests {
    use super::*;

    #[tokio::test]
    async fn rewind_revision_survives_sequence_reuse_and_restart() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        for text in ["kept one", "kept two", "removed one", "removed two"] {
            engine
                .0
                .append(
                    &rt,
                    EntryBody::User {
                        content: vec![ContentPart::text(text)],
                        author: None,
                    },
                )
                .unwrap();
        }
        let before = engine.0.snapshot(&id, 0).unwrap();
        assert_eq!(before.history_revision, 0);
        engine.0.rewind(&id, 2, false, true, false).unwrap();
        for text in ["replacement one", "replacement two", "new tail"] {
            engine
                .0
                .append(
                    &rt,
                    EntryBody::User {
                        content: vec![ContentPart::text(text)],
                        author: None,
                    },
                )
                .unwrap();
        }
        let incremental = engine.0.snapshot(&id, before.info.last_seq).unwrap();
        assert!(incremental.info.last_seq > before.info.last_seq);
        assert_eq!(incremental.history_revision, 1);
        assert_eq!(
            incremental.entries.len(),
            1,
            "a tail alone cannot repair reused seqs"
        );
        let reopened = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        assert_eq!(reopened.0.snapshot(&id, 0).unwrap().history_revision, 1);
        let mut legacy = serde_json::to_value(before).unwrap();
        legacy.as_object_mut().unwrap().remove("history_revision");
        assert_eq!(
            serde_json::from_value::<SessionSnapshot>(legacy)
                .unwrap()
                .history_revision,
            0
        );
    }
}

#[cfg(all(test, unix))]
mod object_read_tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn rewind_skips_a_swapped_protected_target_without_reading_or_restoring_it() {
        use std::os::unix::fs::symlink;
        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        let path = project.path().join("selected.txt");
        let protected = home.path().join("protected.txt");
        std::fs::write(&path, b"ordinary last edit").unwrap();
        std::fs::write(&protected, b"protected rewind sentinel").unwrap();
        let name = path.to_string_lossy().into_owned();
        let snapshot = |bytes: &[u8], after: &[u8]| FileSnapshot {
            id: 0,
            session_id: rt.st.lock().info.id.clone(),
            seq: 0,
            path: name.clone(),
            content: Some(bytes.to_vec()),
            after_hash: Some(crate::util::sha256_hex(after)),
        };
        let snaps = BTreeMap::from([(
            name.clone(),
            (
                snapshot(b"ordinary before edit", b"protected rewind sentinel"),
                snapshot(b"ordinary before edit", b"protected rewind sentinel"),
            ),
        )]);
        std::fs::remove_file(&path).unwrap();
        symlink(&protected, &path).unwrap();
        let mut res = RewindResult {
            session: rt.st.lock().info.clone(),
            to_seq: 0,
            dry_run: false,
            restored: Vec::new(),
            deleted: Vec::new(),
            skipped: Vec::new(),
            not_undone: Vec::new(),
            notes: Vec::new(),
            removed_entries: 0,
            branch: None,
        };
        engine
            .0
            .restore_files(&snaps, false, &str::to_string, &mut res);
        assert!(res.restored.is_empty());
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(
            std::fs::read(protected).unwrap(),
            b"protected rewind sentinel"
        );
        // Ordinary binary restore still uses exact bytes rather than text.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"current\0binary").unwrap();
        let snaps = BTreeMap::from([(
            name.clone(),
            (
                snapshot(b"old\0binary", b"current\0binary"),
                snapshot(b"old\0binary", b"current\0binary"),
            ),
        )]);
        engine
            .0
            .restore_files(&snaps, false, &str::to_string, &mut res);
        assert_eq!(std::fs::read(path).unwrap(), b"old\0binary");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_edit_snapshot_hashes_never_read_swapped_protected_files() {
        use std::os::unix::fs::symlink;
        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        let sid = rt.st.lock().info.id.clone();
        let path = project.path().join("edited.txt");
        let protected = home.path().join("protected.txt");
        std::fs::write(&path, b"ordinary").unwrap();
        std::fs::write(&protected, b"protected hash sentinel").unwrap();
        engine
            .0
            .store
            .snapshot_file(&sid, 0, &path.to_string_lossy(), Some(b"before"))
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&protected, &path).unwrap();
        engine.0.seal_snapshots(&sid, 0).unwrap();
        assert_eq!(
            engine.0.store.open_snapshots(&sid, 0).unwrap().len(),
            1,
            "unsafe path got hashed"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"ordinary").unwrap();
        engine.0.seal_snapshots(&sid, 0).unwrap();
        assert!(engine.0.store.open_snapshots(&sid, 0).unwrap().is_empty());
    }
}
