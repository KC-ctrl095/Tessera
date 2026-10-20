//! Work handed to a bot in the background (`message_bot` without `wait`):
//! the bot works in its own conversation while the chat that asked goes on
//! (and stays free for the user). The bot's conversation knows whom it
//! works for (`SessionInfo::working_for`, which the apps show); when its
//! turn ends, its reply goes back into that chat as a message from the bot,
//! and the chat continues with it, as background subagents' reports do.
//! Stopping that chat stops the bot's work too; stopping the bot's
//! conversation sends back that it was stopped (and the chat waits for the
//! user). A restart in between: the bot's work goes on after a planned
//! restart and reports as usual; otherwise the chat hears it was cut off.

use std::sync::Arc;

use tessera_proto::{ContentPart, EntryBody, WorkingFor};
use tokio_util::sync::CancellationToken;

use super::{Inner, Rt, Waiting, is_room};
use crate::store::StopMarks;
use crate::tools::HandedOff;
use crate::util::{new_id, now_ms};

/// "12 s", "4 min", "2 h 5 min": how long the bot worked.
fn took(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// The header of a bot's reply delivered into a chat (the apps read it).
pub(super) fn reply_header(handle: &str, took_ms: u64, dm: &str) -> String {
    format!("[Reply from @{handle} · {} · session {dm}]", took(took_ms))
}

impl Inner {
    /// Whether `caller` may hand work to a bot in the background: not a
    /// subagent, a scheduled job (its turn ending ends the job) or a bot
    /// already answering another bot (those wait for the reply), nor a room
    /// (its bots take turns).
    pub(super) fn may_hand_off_in_background(&self, caller: &str) -> bool {
        let Ok(rt) = self.rt(caller) else {
            return false;
        };
        let st = rt.st.lock();
        st.depth == 0
            && st.bot_depth == 0
            && st.info.parent_id.is_none()
            && !is_room(&st.info)
            && !matches!(
                st.info.kind,
                tessera_proto::SessionKind::Job | tessera_proto::SessionKind::Subagent
            )
    }

    /// Hand `message` to the bot `target` and return at once: the bot works
    /// on it in its conversation, and its reply comes back into `caller`
    /// when it is done (see the module notes).
    pub(super) fn hand_to_bot(
        self: &Arc<Self>,
        caller: &str,
        target: &str,
        message: String,
        call_id: &str,
    ) -> Result<HandedOff, String> {
        let started = self.start_bot_message(caller, target, message)?;
        // Stoppable from the moment it is handed over (the run takes the
        // same token when it starts).
        let cancel = CancellationToken::new();
        let live = self.stops_here();
        // The bot's work belongs to the caller's turn (Stops included).
        let caller_stops = self.rt(caller).ok().map(|r| r.st.lock().turn_stops.clone());
        {
            let mut st = started.dm.st.lock();
            st.next_turn_stops = caller_stops;
            st.info.working_for = Some(WorkingFor {
                session_id: caller.to_string(),
                call_id: call_id.to_string(),
                since: now_ms(),
                from_seq: started.from_seq,
            });
            st.cancel = Some(cancel.clone());
            st.turn_stops = st.next_turn_stops.clone().unwrap_or(live);
        }
        self.save(&started.dm);
        let me = self.clone();
        let dm = started.dm.clone();
        tokio::spawn(async move { me.run(dm, cancel).await });
        Ok(HandedOff {
            session_id: started.dm_id,
            bot: format!("@{}", started.to.handle),
        })
    }

    /// The bot's turn in `dm` ended (`run`): its reply, or why there is
    /// none, goes to the chat it worked for. That chat continues with it,
    /// unless the user stopped the bot.
    pub(super) fn deliver_bot_reply(
        self: &Arc<Self>,
        dm: &Rt,
        wf: WorkingFor,
        attempt: Option<crate::store::Attempt>,
    ) {
        let (dm_id, bot_id, stops) = {
            let st = dm.st.lock();
            // The caller's turn the bot worked for (its Stops).
            (
                st.info.id.clone(),
                st.info.bot_id.clone(),
                st.turn_stops.clone(),
            )
        };
        let bot = bot_id
            .as_deref()
            .and_then(|id| self.all_bots().into_iter().find(|b| b.id == id));
        let handle = bot.as_ref().map_or("bot", |b| b.handle.as_str());
        let terminal = attempt.as_ref().and_then(|a| a.outcome);
        let stopped = matches!(
            terminal,
            Some(crate::store::AttemptOutcome::Stopped | crate::store::AttemptOutcome::Interrupted)
        );
        let body = match bot.as_ref() {
            Some(_) => match self.bot_answer(&dm_id, attempt.as_ref()) {
                Ok(answer) => self.fit_reply(&wf.session_id, &answer),
                Err(notes) => format!(
                    "@{handle} did not finish: {}",
                    self.fit_reply(&wf.session_id, &notes)
                ),
            },
            None => format!("@{handle} was removed before it replied."),
        };
        let text = format!(
            "{}\n\n{body}",
            reply_header(handle, now_ms().saturating_sub(wf.since), &dm_id)
        );
        self.deliver_to_chat(&wf.session_id, bot_id, text, (!stopped).then_some(stops));
    }

    /// A reply too long for the chat's context: its start and end, the whole
    /// of it kept as an artifact to read with `recall`.
    fn fit_reply(&self, caller: &str, answer: &str) -> String {
        let max = self.tool_output_limit(caller);
        if answer.len() <= max {
            return answer.to_string();
        }
        let (view, _) = crate::util::head_tail(answer, max, 0.3);
        match self
            .store
            .put_artifact(Some(caller), "bot reply", answer.as_bytes())
        {
            Ok(id) => format!(
                "{view}\n\n[Reply shortened. The whole of it ({} bytes) is artifact `{id}`; use recall with artifact=\"{id}\" and offset/grep to see more.]",
                answer.len()
            ),
            Err(_) => view,
        }
    }

    /// A message from `author` (a bot) into the chat `caller`: taken up at
    /// its next step while it works, else added, and (with `resume`: the
    /// Stops of the turn it belongs to) the chat continues with it, unless
    /// the user pressed Stop here since: then it waits for them.
    fn deliver_to_chat(
        self: &Arc<Self>,
        caller: &str,
        author: Option<String>,
        text: String,
        resume: Option<StopMarks>,
    ) {
        let Ok(rt) = self.rt(caller) else {
            return;
        };
        let content = vec![ContentPart::text(text)];
        {
            let mut st = rt.st.lock();
            let stops = resume.clone().unwrap_or_else(|| st.turn_stops.clone());
            if st.running {
                st.steer.push(Waiting {
                    id: new_id(),
                    content,
                    author,
                    stops,
                });
                self.queue_changed(&st);
                return;
            }
        }
        if let Err(e) = self.append(&rt, EntryBody::User { content, author }) {
            tracing::warn!("a bot's reply could not reach chat {caller}: {e}");
            return;
        }
        if let Some(stops) = resume.filter(|s| self.running_here_since(s)) {
            self.clone().resume_at(rt, Some(stops));
        }
    }

    /// Stop the bots working for `caller` (it was stopped or deleted).
    pub(super) fn stop_bot_handoffs(&self, caller: &str) -> usize {
        let live: Vec<Arc<Rt>> = self.sessions.lock().values().cloned().collect();
        let mut n = 0;
        for rt in live {
            let st = rt.st.lock();
            if st.running
                && st
                    .info
                    .working_for
                    .as_ref()
                    .is_some_and(|w| w.session_id == caller)
                && let Some(c) = &st.cancel
            {
                c.cancel();
                n += 1;
            }
        }
        n
    }

    /// At start: a bot's work that stopped with the server and doesn't go on
    /// by itself (`mark_interrupted`): the chat it worked for hears so.
    pub(super) fn handoff_cut_off(
        &self,
        info: &mut tessera_proto::SessionInfo,
        internal: &crate::store::SessionInternal,
    ) {
        let Some(wf) = info.working_for.take() else {
            return;
        };
        let handle = info
            .bot_id
            .as_deref()
            .and_then(|id| self.all_bots().into_iter().find(|b| b.id == id))
            .map_or_else(|| "bot".to_string(), |b| b.handle);
        let entries = self.store.entries(&info.id, 0).unwrap_or_default();
        let notes = super::attempts::history(&entries, internal.attempts.last());
        let text = format!(
            "{}\n\n@{handle} did not finish: {notes}",
            reply_header(&handle, now_ms().saturating_sub(wf.since), &info.id)
        );
        let _ = self.store.append(
            &wf.session_id,
            EntryBody::User {
                content: vec![ContentPart::text(text)],
                author: info.bot_id.clone(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restart_bot_delivery_uses_interrupted_attempt_and_keeps_partial_notes() {
        use tessera_proto::{AssistantPart, SessionKind, SessionStatus, StopReason};
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let dm = engine
            .0
            .create_session(
                None,
                None,
                Some("Restart bot fixture".into()),
                SessionKind::Channel,
                None,
                None,
                None,
            )
            .unwrap();
        dm.st.lock().bot_depth = 1;
        let attempt_id = engine.0.begin_attempt(&dm).unwrap();
        engine
            .0
            .append(
                &dm,
                EntryBody::Assistant {
                    content: vec![AssistantPart::Text {
                        text: "Restarted bot partial notes.".into(),
                    }],
                    stop: StopReason::Error,
                    model: tessera_proto::ModelRef {
                        provider: "p".into(),
                        model: "m".into(),
                    },
                    usage: Default::default(),
                    author: None,
                    error: None,
                },
            )
            .unwrap();
        let parent_id = parent.st.lock().info.id.clone();
        {
            let mut st = dm.st.lock();
            st.info.working_for = Some(WorkingFor {
                session_id: parent_id.clone(),
                call_id: "restart-handoff".into(),
                since: now_ms(),
                from_seq: 0,
            });
        }
        engine.0.set_status(&dm, SessionStatus::Running);
        let id = dm.st.lock().info.id.clone();
        let end = dm.st.lock().info.last_seq;
        engine.0.mark_interrupted(false).unwrap();
        let (_, saved) = engine.0.store.get_session(&id).unwrap().unwrap();
        let a = saved.attempts.iter().find(|a| a.id == attempt_id).unwrap();
        assert_eq!(a.outcome, Some(crate::store::AttemptOutcome::Interrupted));
        assert_eq!(a.to_seq, Some(end));
        let entries = engine.0.store.entries(&parent_id, 0).unwrap();
        let text = entries
            .iter()
            .find_map(|e| {
                if let EntryBody::User { content, author: _ } = &e.body {
                    Some(tessera_proto::plain_text(content))
                } else {
                    None
                }
            })
            .unwrap();
        assert!(
            text.contains("This attempt was interrupted by a server restart"),
            "{text}"
        );
        assert!(
            text.contains("Partial notes, not a final reply or report:"),
            "{text}"
        );
        assert!(text.contains("Restarted bot partial notes."), "{text}");
        assert!(!text.contains("replied:"), "{text}");
    }

    #[test]
    fn the_reply_header_says_who_how_long_and_where() {
        assert_eq!(
            reply_header("orion", 297_000, "s1"),
            "[Reply from @orion · 4 min · session s1]"
        );
        assert_eq!(took(12_400), "12 s");
        assert_eq!(took(2 * 3_600_000 + 5 * 60_000), "2 h 5 min");
    }
}
