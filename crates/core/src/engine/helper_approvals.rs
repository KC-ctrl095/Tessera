//! A helper waiting on an approval is never invisible to the chat that
//! started it: that chat is told at once (a short harness message its model
//! reads, waking it if idle), `agents status` shows the wait, and a parent
//! that could do the same itself without asking may answer it (`agents
//! approve|decline`). Otherwise the user's card stays as it is, and the
//! usual 15-minute expiry tells the helper plainly that nothing ran.

use super::*;

/// A helper Tessera runs for another chat (not Claude Code's own subagents,
/// whose approvals are their chat's).
pub(super) fn is_helper(st: &RtState) -> bool {
    st.info.kind == SessionKind::Subagent && st.internal.subagent.is_some()
}

/// What the chat that started a helper would need to do the helper's action
/// itself: the permission modes in which it runs there without asking, and
/// the "Allow for this session" keys that would cover it there. Checked
/// against that chat's current mode and grants when it answers, not kept as
/// a verdict from when the helper asked.
#[derive(Clone, Debug)]
pub(super) struct ParentAuthority {
    pub(super) unasked: Vec<PermissionMode>,
    pub(super) keys: Vec<String>,
}

impl ParentAuthority {
    /// The modes a chat could be in, for working out in which the action
    /// would run unasked (Read-only never answers for anyone).
    pub(super) const MODES: [PermissionMode; 3] = [
        PermissionMode::Default,
        PermissionMode::Ask,
        PermissionMode::Auto,
    ];
}

impl Inner {
    /// Whether `parent` may answer an approval with this authority now: its
    /// current mode runs the action unasked, or the user allowed it there
    /// for the session.
    fn parent_may_now(&self, parent: &str, authority: Option<&ParentAuthority>) -> bool {
        let Some(a) = authority else {
            return false;
        };
        let Some(prt) = self.live_session(parent) else {
            return false;
        };
        let st = prt.st.lock();
        // No keys: nothing a grant could be about (never "all of none").
        a.unasked.contains(&self.effective_permission(&st))
            || (!a.keys.is_empty() && Self::session_allowed(&st, &a.keys))
    }

    /// A helper now waits for an approval: tell the chat that started it, as
    /// its next step's input, or in a turn of its own when it is idle.
    pub(super) fn tell_parent_of_approval(
        &self,
        rt: &Rt,
        request: &ApprovalRequest,
        authority: Option<&ParentAuthority>,
    ) {
        let (parent, name, marks) = {
            let st = rt.st.lock();
            if !is_helper(&st) || st.info.internal {
                return;
            }
            let Some(parent) = st.info.parent_id.clone() else {
                return;
            };
            (parent, helper_name(&st), st.turn_stops.clone())
        };
        let Some(parent_rt) = self.live_session(&parent) else {
            return;
        };
        let may = self.parent_may_now(&parent, authority);
        let text = parent_note(
            &name,
            &short_id(&request.id),
            &self.approval_what(request),
            may,
        );
        let note = Waiting {
            id: new_id(),
            content: vec![ContentPart::text(text)],
            author: Some("harness".to_string()),
            stops: marks.clone(),
        };
        if !matches!(self.append_or_steer(&parent_rt, note), Ok(Some(_))) {
            return;
        }
        if self.running_here_since(&marks)
            && let Some(me) = self.me.upgrade()
        {
            me.resume_at(parent_rt, Some(marks));
        }
    }

    /// Something for a chat's agent from elsewhere (a helper's report, a
    /// note): its next step's input while it works, else an entry in its
    /// history (`Some`). Decided and kept in the store under the chat's
    /// lock, so a turn starting meanwhile reads it, or takes it as steering:
    /// never one that read the history just before it came.
    pub(super) fn append_or_steer(&self, rt: &Rt, w: Waiting) -> Result<Option<Entry>> {
        let (id, entry) = {
            let mut st = rt.st.lock();
            if st.running {
                st.steer.push(w);
                self.queue_changed(&st);
                return Ok(None);
            }
            let id = st.info.id.clone();
            let body = EntryBody::User {
                content: w.content,
                author: w.author,
            };
            let entry = self.store.append(&id, body)?;
            (id, entry)
        };
        self.record_entry(rt, id, entry).map(Some)
    }

    /// What an approval is for, in one line, for a model to read (secrets
    /// the user saved are taken out).
    fn approval_what(&self, request: &ApprovalRequest) -> String {
        let what = one_line(&format!("{} {}", request.tool, request.summary), 160);
        crate::tools::redact_known(&what, &self.redaction_secrets())
    }

    /// The approvals `parent`'s helpers wait for, oldest first: helper
    /// session, approval id, request, authority and seconds left. Copied out
    /// first: no session lock is taken while holding the approvals' lock.
    fn helper_pending(
        &self,
        parent: &str,
    ) -> Vec<(
        String,
        String,
        ApprovalRequest,
        Option<ParentAuthority>,
        u64,
    )> {
        let now = tokio::time::Instant::now();
        let mut waiting: Vec<_> = self
            .approvals
            .lock()
            .iter()
            .map(|(id, p)| {
                (
                    p.deadline,
                    (
                        p.acting_on.clone(),
                        id.clone(),
                        p.request.clone(),
                        p.authority.clone(),
                        p.deadline.saturating_duration_since(now).as_secs(),
                    ),
                )
            })
            .collect();
        waiting.sort_by_key(|(deadline, _)| *deadline);
        waiting
            .into_iter()
            .map(|(_, w)| w)
            .filter(|(child, ..)| {
                self.live_session(child)
                    .is_some_and(|rt| rt.st.lock().info.parent_id.as_deref() == Some(parent))
            })
            .collect()
    }

    /// For `agents status`: what each of `parent`'s helpers waits on, by
    /// helper session.
    pub(super) fn helper_waits(&self, parent: &str) -> HashMap<String, String> {
        let mut out: HashMap<String, String> = HashMap::new();
        for (child, id, request, authority, left) in self.helper_pending(parent) {
            let who = if self.parent_may_now(parent, authority.as_ref()) {
                "you may approve or decline it"
            } else {
                "only the user can approve it; you may decline it"
            };
            let line = format!(
                "waits for approval {}: {} ({} min left; {who})",
                short_id(&id),
                self.approval_what(&request),
                left.div_ceil(60)
            );
            out.entry(child)
                .and_modify(|l| {
                    l.push_str("\n  ");
                    l.push_str(&line);
                })
                .or_insert(line);
        }
        out
    }

    /// `agents approve|decline`: answer an approval `which` (one of
    /// `parent`'s helpers) waits for: the one `pick` names (its id from the
    /// note or status), else the oldest (to approve: the oldest it may
    /// approve). Approving needs the parent's own authority now; declining
    /// is always the parent's to do. The answer names the action.
    pub(super) fn answer_helper_approval(
        &self,
        parent: &str,
        which: &str,
        allow: bool,
        pick: Option<&str>,
    ) -> Result<String, String> {
        let child = self.find_child(parent, which)?;
        let name = self
            .live_session(&child)
            .map(|rt| helper_name(&rt.st.lock()))
            .unwrap_or_else(|| which.to_string());
        let pick = pick
            .map(|p| p.trim().trim_start_matches('#'))
            .filter(|p| !p.is_empty());
        let mine: Vec<_> = self
            .helper_pending(parent)
            .into_iter()
            .filter(|(c, ..)| *c == child)
            .collect();
        if mine.is_empty() {
            return Err(format!("`{name}` isn't waiting for an approval."));
        }
        let chosen = match pick {
            Some(p) => mine
                .iter()
                .find(|(_, id, ..)| short_id(id) == p || id.as_str() == p)
                .ok_or_else(|| {
                    format!("`{name}` has no approval `{p}` waiting; `agents status` lists them.")
                })?,
            None if allow => mine
                .iter()
                .find(|(.., a, _)| self.parent_may_now(parent, a.as_ref()))
                .unwrap_or(&mine[0]),
            None => &mine[0],
        };
        let (_, id, request, authority, _) = chosen;
        let what = self.approval_what(request);
        if allow && !self.parent_may_now(parent, authority.as_ref()) {
            return Err(format!(
                "You can't approve `{name}`'s {what} ({}): in your own chat it would need the user's approval too. The user's card stays open; you can decline it (`agents` decline), or tell the user.",
                short_id(id)
            ));
        }
        let Some(pending) = self.approvals.lock().remove(id) else {
            return Err("That approval was answered a moment ago.".into());
        };
        if tokio::time::Instant::now() >= pending.deadline {
            let _ = pending.tx.send(Answer::rule(ApprovalOutcome::Expired));
            return Err(format!(
                "That approval had expired: `{name}` was told its {what} did not run."
            ));
        }
        let outcome = if allow {
            ApprovalOutcome::Allowed(ApprovalDecision::Allow)
        } else {
            ApprovalOutcome::DeclinedByParent
        };
        let _ = pending.tx.send(Answer {
            outcome,
            who: format!("parent:{parent}"),
        });
        let left = mine.len() - 1;
        let more = if left > 0 {
            format!(" {left} more waiting (`agents status`).")
        } else {
            String::new()
        };
        Ok(if allow {
            format!(
                "Approved `{name}`'s {what} ({}); it goes on.{more}",
                short_id(id)
            )
        } else {
            format!(
                "Declined `{name}`'s {what} ({}); it is told and carries on without it.{more}",
                short_id(id)
            )
        })
    }
}

/// An approval's short name for a model to quote back (`agents` text).
fn short_id(id: &str) -> String {
    let tail: String = id.chars().rev().take(6).collect();
    tail.chars().rev().collect()
}

fn helper_name(st: &RtState) -> String {
    st.internal
        .subagent
        .as_ref()
        .map(|s| s.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| st.info.title.clone())
}

/// What the chat that started a helper reads when the helper waits.
fn parent_note(name: &str, id: &str, what: &str, parent_may: bool) -> String {
    if parent_may {
        format!(
            "[Helper \"{name}\" waits for approval {id}: {what}. You could do this yourself without asking, so you may answer it: `agents` approve or decline with name \"{name}\" and text \"{id}\". Otherwise the user decides; unanswered, it expires in 15 min and the helper is told it didn't run.]"
        )
    } else {
        format!(
            "[Helper \"{name}\" waits for the user's approval {id}: {what}. Your chat would have to ask for this too, so only the user can approve it; you may decline it (`agents` decline, name \"{name}\", text \"{id}\") or carry on. Unanswered, it expires in 15 min and the helper is told it didn't run.]"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_is_told_what_waits_and_what_it_may_do() {
        let may = parent_note("Review", "a1b2c3", "bash cargo test", true);
        assert!(may.contains("Helper \"Review\" waits for approval a1b2c3: bash cargo test"));
        assert!(may.contains("agents` approve or decline"));
        assert!(may.contains("text \"a1b2c3\""));
        assert!(may.contains("15 min"));
        let user = parent_note("Review", "a1b2c3", "bash rm -rf build", false);
        assert!(user.contains("only the user can approve it"));
        assert!(user.contains("decline"));
        assert!(!user.contains("approve or decline"));
        assert_eq!(short_id("01m4qsm7c8q0eqmf0frnk1"), "0frnk1");
    }

    fn request(summary: &str) -> ApprovalRequest {
        ApprovalRequest {
            id: new_id(),
            call_id: new_id(),
            tool: "bash".into(),
            summary: summary.into(),
            reason: "deletes untracked files".into(),
            arguments: json!({"command": summary}),
            once: false,
        }
    }

    async fn pending(engine: &Engine, child: &str, n: usize) {
        for _ in 0..200 {
            if engine
                .0
                .approvals
                .lock()
                .values()
                .filter(|p| p.acting_on == child)
                .count()
                == n
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("not {n} approvals pending");
    }

    fn set_mode(engine: &Engine, rt: &Rt, mode: PermissionMode) {
        let mut st = rt.st.lock();
        st.info.permission = mode;
        engine.0.publish_permission(&st);
    }

    /// A parent and a helper started by it, in a project folder (the data
    /// folder is no place for a helper); the parent is in a turn, so notes
    /// are its next step's input.
    fn family(engine: &Engine, project: &Path) -> (Arc<Rt>, Arc<Rt>, String, String, String) {
        let parent = engine
            .0
            .create_session(
                Some(project.display().to_string()),
                None,
                Some("Parent".into()),
                SessionKind::Chat,
                None,
                Some(PermissionMode::Default),
                None,
            )
            .unwrap();
        let child = engine.0.start_lock_order_test_child(&parent);
        let (pid, cid, name) = {
            let st = child.st.lock();
            (
                st.info.parent_id.clone().unwrap(),
                st.info.id.clone(),
                helper_name(&st),
            )
        };
        parent.st.lock().running = true;
        (parent, child, pid, cid, name)
    }

    fn ask(
        engine: &Engine,
        child: &Arc<Rt>,
        pid: &str,
        req: ApprovalRequest,
        unasked: Vec<PermissionMode>,
    ) -> tokio::task::JoinHandle<ApprovalOutcome> {
        let (me, child, pid) = (engine.0.clone(), child.clone(), pid.to_string());
        let authority = Some(ParentAuthority {
            unasked,
            keys: vec!["exec:/fixture:git clean -nf".into()],
        });
        tokio::spawn(async move {
            me.ask_approval_as(&child, &pid, req, &CancellationToken::new(), authority)
                .await
        })
    }

    #[tokio::test]
    async fn a_parent_answers_its_helpers_wait_only_with_the_authority() {
        let (engine, _dir, _) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let (parent, child, pid, cid, name) = family(&engine, project.path());
        // The parent (Default) would run this unasked itself: it may approve.
        let first = ask(
            &engine,
            &child,
            &pid,
            request("git clean -nf"),
            vec![PermissionMode::Default],
        );
        pending(&engine, &cid, 1).await;
        let told: Vec<String> = parent
            .st
            .lock()
            .steer
            .iter()
            .filter(|w| w.author.as_deref() == Some("harness"))
            .map(|w| tessera_proto::plain_text(&w.content))
            .collect();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].contains(&format!("Helper \"{name}\" waits for approval ")),
            "{told:?}"
        );
        assert!(told[0].contains(": bash git clean -nf"), "{told:?}");
        let status = engine.0.agents_status(&pid);
        assert!(
            status.contains("waits for approval ")
                && status.contains(": bash git clean -nf")
                && status.contains("you may approve or decline it"),
            "{status}"
        );
        let said = engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap();
        assert!(
            said.starts_with("Approved") && said.contains("bash git clean -nf"),
            "{said}"
        );
        assert_eq!(
            first.await.unwrap(),
            ApprovalOutcome::Allowed(ApprovalDecision::Allow)
        );
        // Only YOLO would run it unasked: approving is refused and the card
        // stays; declining is the parent's to do.
        let second = ask(
            &engine,
            &child,
            &pid,
            request("git stash drop"),
            vec![PermissionMode::Auto],
        );
        pending(&engine, &cid, 1).await;
        let refused = engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap_err();
        assert!(
            refused.contains("can't approve") && refused.contains("git stash drop"),
            "{refused}"
        );
        assert!(
            engine
                .0
                .approvals
                .lock()
                .values()
                .any(|p| p.acting_on == cid)
        );
        assert!(
            engine
                .0
                .agents_status(&pid)
                .contains("only the user can approve it; you may decline it")
        );
        engine
            .0
            .answer_helper_approval(&pid, &name, false, None)
            .unwrap();
        let outcome = second.await.unwrap();
        assert_eq!(outcome, ApprovalOutcome::DeclinedByParent);
        let told = outcome.message_in("bash", true);
        assert!(
            told.contains("The chat that started you declined"),
            "{told}"
        );
        // Nothing waits now.
        let none = engine
            .0
            .answer_helper_approval(&pid, &name, false, None)
            .unwrap_err();
        assert!(none.contains("isn't waiting"), "{none}");
        assert!(!engine.0.agents_status(&pid).contains("waits for approval"));
        // Another chat can't answer it.
        let third = ask(
            &engine,
            &child,
            &pid,
            request("git clean -nf"),
            vec![PermissionMode::Default],
        );
        pending(&engine, &cid, 1).await;
        assert!(
            engine
                .0
                .answer_helper_approval("someone-else", &name, true, None)
                .is_err()
        );
        engine
            .0
            .answer_helper_approval(&pid, &name, false, None)
            .unwrap();
        assert_eq!(third.await.unwrap(), ApprovalOutcome::DeclinedByParent);
    }

    /// The parent's authority is its authority when it answers: lowered
    /// meanwhile (YOLO → Default), it may no longer approve; raised (Default
    /// → YOLO), it may; and a "this session" grant there counts too.
    #[tokio::test]
    async fn the_parents_authority_is_checked_when_it_answers() {
        let (engine, _dir, _) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let (parent, child, pid, cid, name) = family(&engine, project.path());
        set_mode(&engine, &parent, PermissionMode::Auto);
        let first = ask(
            &engine,
            &child,
            &pid,
            request("git clean -nf"),
            vec![PermissionMode::Auto],
        );
        pending(&engine, &cid, 1).await;
        assert!(
            engine
                .0
                .agents_status(&pid)
                .contains("you may approve or decline it")
        );
        // The user lowers the parent before it answers.
        set_mode(&engine, &parent, PermissionMode::Default);
        let refused = engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap_err();
        assert!(refused.contains("can't approve"), "{refused}");
        assert!(
            engine
                .0
                .agents_status(&pid)
                .contains("only the user can approve it")
        );
        // A YOLO parent may approve (its mode raised again meanwhile).
        set_mode(&engine, &parent, PermissionMode::Auto);
        engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap();
        assert_eq!(
            first.await.unwrap(),
            ApprovalOutcome::Allowed(ApprovalDecision::Allow)
        );
        // In Default, a grant the user gave in the parent for the session counts.
        set_mode(&engine, &parent, PermissionMode::Default);
        let second = ask(
            &engine,
            &child,
            &pid,
            request("git clean -nf"),
            vec![PermissionMode::Auto],
        );
        pending(&engine, &cid, 1).await;
        assert!(
            engine
                .0
                .answer_helper_approval(&pid, &name, true, None)
                .is_err()
        );
        Inner::allow_for_session(
            &mut parent.st.lock(),
            vec!["exec:/fixture:git clean -nf".into()],
        );
        engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap();
        assert_eq!(
            second.await.unwrap(),
            ApprovalOutcome::Allowed(ApprovalDecision::Allow)
        );
    }

    /// With several waiting, the id from the note picks one; without it,
    /// approve takes the oldest the parent may approve, and the answer names
    /// the action it answered.
    #[tokio::test]
    async fn the_id_from_the_note_picks_which_approval_is_answered() {
        let (engine, _dir, _) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let (_parent, child, pid, cid, name) = family(&engine, project.path());
        let user_only = request("git stash drop");
        let mine = request("git clean -nf");
        let mine_id = short_id(&mine.id);
        let a = ask(&engine, &child, &pid, user_only, vec![PermissionMode::Auto]);
        pending(&engine, &cid, 1).await;
        let b = ask(&engine, &child, &pid, mine, vec![PermissionMode::Default]);
        pending(&engine, &cid, 2).await;
        let status = engine.0.agents_status(&pid);
        assert!(
            status.contains(&format!("waits for approval {mine_id}: bash git clean -nf")),
            "{status}"
        );
        // The oldest is the user's to approve: approve takes the one it may.
        let said = engine
            .0
            .answer_helper_approval(&pid, &name, true, None)
            .unwrap();
        assert!(
            said.contains("git clean -nf") && said.contains("1 more waiting"),
            "{said}"
        );
        assert_eq!(
            b.await.unwrap(),
            ApprovalOutcome::Allowed(ApprovalDecision::Allow)
        );
        // A wrong id answers nothing.
        let wrong = engine
            .0
            .answer_helper_approval(&pid, &name, false, Some("zzzzzz"))
            .unwrap_err();
        assert!(wrong.contains("no approval `zzzzzz`"), "{wrong}");
        let left = engine.0.helper_pending(&pid);
        let left_id = short_id(&left[0].1);
        let said = engine
            .0
            .answer_helper_approval(&pid, &name, false, Some(&format!("#{left_id}")))
            .unwrap();
        assert!(
            said.starts_with("Declined") && said.contains("git stash drop"),
            "{said}"
        );
        assert_eq!(a.await.unwrap(), ApprovalOutcome::DeclinedByParent);
    }
}
