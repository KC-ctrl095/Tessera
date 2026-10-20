//! What the notification center hears about chats and scheduled jobs: an
//! approval that waits, helpers' reports, a turn that ran a while, and each
//! run of a scheduled job (how it ended, logged and kept per run), plus the
//! end of a job's schedule (`until`, `max_runs`, or a run saying its goal
//! was met). The rows themselves are in `store/notify.rs`.

use std::future::Future;
use std::time::Duration;

use super::notify::plain_line;
use super::*;
use crate::store::{NewNotification, Replace};

/// An approval or question answered within this long was answered by
/// someone watching: it never reaches the notification center.
pub(super) const WAITING_NEWS_AFTER: Duration = Duration::from_secs(2);
/// A chat's turn at least this long that ends is listed (a long one, over
/// `agent.notify_after_minutes`, is a system notification instead).
const TURN_NEWS_AFTER: Duration = Duration::from_secs(60);

/// How many waits for the user each chat has now.
fn waiting() -> &'static parking_lot::Mutex<HashMap<String, usize>> {
    static WAITING: std::sync::OnceLock<parking_lot::Mutex<HashMap<String, usize>>> =
        std::sync::OnceLock::new();
    WAITING.get_or_init(Default::default)
}

/// One wait for the user ends (answered, stopped or failed): with the last
/// one of its chat, the chat's "needs you" item is read.
struct UserWait<'a> {
    inner: &'a Inner,
    root: String,
}

impl Drop for UserWait<'_> {
    fn drop(&mut self) {
        let last = {
            let mut w = waiting().lock();
            let n = w.entry(self.root.clone()).or_default();
            *n = n.saturating_sub(1);
            let last = *n == 0;
            if last {
                w.remove(&self.root);
            }
            last
        };
        if last {
            self.inner.activity_resolved(&self.root, "approval");
        }
    }
}

/// How a job's run ended, as words and whether the user should hear.
fn job_outcome(outcome: &str) -> Option<(&'static str, bool)> {
    Some(match outcome {
        "ok" => ("Scheduled task finished", false),
        "error" => ("Scheduled task failed", true),
        "paused" => ("Scheduled task paused", true),
        "interrupted" => ("Scheduled task interrupted", true),
        "stopped" => ("Scheduled task stopped", false),
        _ => return None,
    })
}

impl Inner {
    pub(super) fn notification_user_facing(&self, info: &SessionInfo) -> bool {
        if info.internal || info.working_for.is_some() {
            return false;
        }
        if info.kind == SessionKind::Chat {
            return true;
        }
        if !is_room(info) {
            return false;
        }
        // A room round addressed by a bot is not a reply the user awaits.
        self.store
            .entries(&info.id, 0)
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|entry| match &entry.body {
                EntryBody::User { author, .. } if author.as_deref() != Some("harness") => {
                    Some(author.is_none())
                }
                _ => None,
            })
            .unwrap_or(false)
    }

    pub(super) fn notification_action_pending(&self, session: &str) -> bool {
        ["approval", "secret"].iter().any(|kind| {
            self.store
                .unread_notification(session, kind)
                .ok()
                .flatten()
                .is_some()
        })
    }

    pub(super) fn secret_entry_needed(&self, session: &str, entry: &Entry) {
        if !matches!(&entry.body, EntryBody::ToolResult { tool, is_error: false, details: Some(d), .. }
            if tool == "save_secret" && d.get("secret_entry").is_some_and(|e| e.get("saved").and_then(Value::as_bool) != Some(true)))
        {
            return;
        }
        let root = self.root_session(session);
        let _ = self.send_notification(NewNotification {
            title: "Password or sign-in needed".into(),
            body: "Open the chat to enter it securely.".into(),
            from: self.notification_sender(&root),
            session_id: Some(root),
            kind: "secret".into(),
            replace: Replace::Unread,
            ..Default::default()
        });
    }

    /// Wait for the user in `session`'s chat (an approval, a question on a
    /// whiteboard): one that takes a while is listed in the notification
    /// center until it is answered.
    pub(super) async fn wait_for_user<T>(
        &self,
        session: &str,
        title: &str,
        what: &str,
        wait: impl Future<Output = T>,
    ) -> T {
        let root = self.root_session(session);
        *waiting().lock().entry(root.clone()).or_default() += 1;
        let _done = UserWait {
            inner: self,
            root: root.clone(),
        };
        tokio::pin!(wait);
        tokio::select! {
            v = &mut wait => return v,
            _ = tokio::time::sleep(WAITING_NEWS_AFTER) => {}
        }
        if self.user_facing(&root) {
            self.keep_activity(
                &root,
                "approval",
                title,
                plain_line(what, 200),
                Replace::Unread,
            );
        }
        wait.await
    }

    /// The user's own chat (not a job's, a helper's, a bot conversation or
    /// one of the harness's).
    fn user_facing(&self, session: &str) -> bool {
        self.rt(session).is_ok_and(|rt| {
            let st = rt.st.lock();
            st.info.kind == SessionKind::Chat && !st.info.internal && st.depth == 0
        })
    }

    /// A helper's report reached `parent` (its chat): one item per chat
    /// until read, counting the reports.
    pub(super) fn helper_reported(&self, parent: &str, name: &str, finished: bool) {
        let root = self.root_session(parent);
        if !self.user_facing(&root) {
            return;
        }
        let prior = self
            .store
            .unread_notification(&root, "helper")
            .ok()
            .flatten();
        let count = prior.as_ref().map_or(0, |n| {
            n.title
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<u32>().ok())
                .unwrap_or(1)
        }) + 1;
        let title = if count == 1 {
            "A helper reported back".to_string()
        } else {
            format!("{count} helpers reported back")
        };
        let what = if finished {
            plain_line(name, 60)
        } else {
            format!("{} (didn't finish)", plain_line(name, 60))
        };
        let body = match prior {
            Some(p) if !p.body.is_empty() => format!("{}, {what}", p.body),
            _ => what,
        };
        self.keep_activity(
            &root,
            "helper",
            &title,
            plain_line(&body, 300),
            Replace::Unread,
        );
    }

    /// A turn ended and nothing follows it at once (`handoff`: the next
    /// queued message starts now). `began`: when (instant, ms) and from
    /// which entry.
    pub(super) fn turn_settled(
        &self,
        rt: &Rt,
        outcome: &Outcome,
        handoff: bool,
        began: (std::time::Instant, u64, u64),
    ) {
        if handoff {
            return;
        }
        let (info, depth) = {
            let st = rt.st.lock();
            (st.info.clone(), st.depth)
        };
        let (id, kind, user_facing) = (
            info.id.clone(),
            info.kind,
            self.notification_user_facing(&info),
        );
        if depth > 0 || info.internal {
            return;
        }
        match kind {
            SessionKind::Job => self.job_turn_ended(&id, outcome, began.2),
            _ if user_facing => self.chat_turn_ended(&id, outcome, began),
            _ => {}
        }
    }

    /// A chat's turn that ran a while is listed when it ends (unless the
    /// agent told the user itself, or long work already notified).
    fn chat_turn_ended(&self, id: &str, outcome: &Outcome, began: (std::time::Instant, u64, u64)) {
        let took = began.0.elapsed();
        let long = self.config.read().agent.notify_after_minutes;
        let action_needed = matches!(outcome, Outcome::Failed(_)) || self.turn_paused(id, began.2);
        if (took < TURN_NEWS_AFTER && !action_needed && self.last_words(id, began.2).is_empty())
            || (long > 0.0
                && took.as_secs_f64() >= long * 60.0
                && self
                    .rt(id)
                    .is_ok_and(|rt| rt.st.lock().info.kind == SessionKind::Chat))
        {
            return;
        }
        if notify::sent_since(id, began.1) || self.has_delegated_work(id) {
            return;
        }
        let took = human_wait(took);
        let (kind, title, body) = match outcome {
            Outcome::Aborted => return,
            Outcome::Failed(e) => (
                "error",
                "Stopped by an error",
                format!("After {took}: {}", one_line(e, 200)),
            ),
            Outcome::Done if self.turn_paused(id, began.2) => (
                "error",
                "Paused",
                "It stopped short and waits for a message to continue.".to_string(),
            ),
            Outcome::Done => {
                let said = self.last_words(id, began.2);
                if said.is_empty() && self.protocol_only(id, began.2) {
                    return;
                }
                (
                    "finished",
                    "Finished",
                    if said.is_empty() {
                        format!("Worked {took}.")
                    } else {
                        format!("Worked {took}. {said}")
                    },
                )
            }
        };
        self.keep_activity(id, kind, title, body, Replace::Unread);
    }

    /// Only the harness's own exchange (a completion check) since `from`.
    fn protocol_only(&self, id: &str, from: u64) -> bool {
        self.store
            .entries(id, from)
            .unwrap_or_default()
            .iter()
            .any(crate::tools::meta::is_protocol_entry)
    }

    /// The turn stopped short at its step limit or going in circles (its
    /// "Paused after …" notice), waiting for a message.
    fn turn_paused(&self, id: &str, from: u64) -> bool {
        self.store
            .entries(id, from)
            .unwrap_or_default()
            .iter()
            .any(|e| {
                matches!(&e.body, EntryBody::Notice { level: NoticeLevel::Warning, text, .. }
                    if text.starts_with("Paused after"))
            })
    }

    /// Helpers or bots still work for `id`: its work isn't over.
    pub(super) fn has_delegated_work(&self, id: &str) -> bool {
        let helpers = self
            .background
            .lock()
            .get(id)
            .is_some_and(|list| list.iter().any(|a| !a.done && !a.stopped));
        helpers
            || self.live_sessions().iter().any(|other| {
                let st = other.st.lock();
                st.running
                    && st
                        .info
                        .working_for
                        .as_ref()
                        .is_some_and(|w| w.session_id == id)
            })
    }

    // ---- scheduled jobs -------------------------------------------------------

    /// A continued run is running again, not still paused or stopped. Its
    /// original start and run count stay: this is the same scheduled run.
    pub(super) fn job_turn_started(&self, rt: &Rt) {
        let id = {
            let st = rt.st.lock();
            if st.info.kind != SessionKind::Job {
                return;
            }
            st.info.id.clone()
        };
        match self.store.job_run_ended(&id, "running", None) {
            Ok(Some((_, Some(job)))) => {
                let _ = self.events.send(Event::JobUpdated { job });
            }
            Err(e) => tracing::error!("recording that job run {id} continued: {e}"),
            _ => {}
        }
    }

    /// An idle-looking run may still have helpers or bots at work. Starting
    /// the next scheduled run now would overlap the work and its reports.
    pub(super) fn job_is_working(&self, job: &Job) -> bool {
        job.last_session.as_deref().is_some_and(|id| {
            self.live_session(id).is_some_and(|rt| rt.st.lock().running)
                || self.has_delegated_work(id)
        })
    }

    /// A job's run ended its turn: how it went (helpers it started still
    /// working: not over yet; their reports start its next turn).
    fn job_turn_ended(&self, id: &str, outcome: &Outcome, from: u64) {
        let (outcome, detail) = match outcome {
            Outcome::Done if self.turn_paused(id, from) => (
                "paused",
                "It stopped short; send a message in its chat to continue.".to_string(),
            ),
            Outcome::Done if self.has_delegated_work(id) => {
                tracing::info!("job run {id}: waiting for its helpers to report");
                return;
            }
            Outcome::Done => ("ok", self.last_words(id, from)),
            Outcome::Aborted => ("stopped", String::new()),
            Outcome::Failed(e) => ("error", one_line(e, 200)),
        };
        self.job_run_finished(id, outcome, &detail);
    }

    /// The user stopped a job's run that was waiting for its helpers or
    /// bots (`stopped`: how many it stopped): the run is over.
    pub(super) fn job_stopped_while_waiting(&self, rt: &Rt, stopped: usize) {
        let (id, job, running) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                st.info.kind == SessionKind::Job,
                st.running,
            )
        };
        if stopped > 0 && job && !running {
            self.job_run_finished(&id, "stopped", "");
        }
    }

    /// Record how the run in `session` ended (`outcome` as
    /// `Job::last_status`), log it, and tell the notification center: one
    /// item per run (a continued run updates it). A failure, pause or
    /// interruption is a system notification; a run that finished, or that
    /// the user stopped, is only listed.
    pub(super) fn job_run_finished(&self, session: &str, outcome: &str, detail: &str) {
        let now = now_ms();
        let (run, job) = match self.store.job_run_ended(session, outcome, Some(now)) {
            Ok(Some((run, job))) => (Some(run), job),
            // A run from before runs were recorded.
            Ok(None) => (None, self.legacy_job_status(session, outcome, now)),
            Err(e) => {
                tracing::error!("recording how job run {session} ended: {e}");
                return;
            }
        };
        let job = job.or_else(|| {
            let id = run.as_ref()?.job_id.clone();
            self.store.jobs().ok()?.into_iter().find(|j| j.id == id)
        });
        if let Some(j) = &job {
            let _ = self.events.send(Event::JobUpdated { job: j.clone() });
        }
        let name = job
            .as_ref()
            .map(|j| j.name.clone())
            .unwrap_or_else(|| "A scheduled task".into());
        let took = run
            .as_ref()
            .map(|r| human_wait(Duration::from_millis(now.saturating_sub(r.started_at))))
            .unwrap_or_else(|| "?".into());
        tracing::info!(job_id = job.as_ref().map_or("?", |j| j.id.as_str()), session_id = session, outcome, elapsed = %took, "job ended");
        let Some((title, alert)) = job_outcome(outcome) else {
            return;
        };
        // Routine results are already in the task's run history. They are
        // not news and must not replace a meaningful explicit notify.
        if !alert {
            let source = self.job_notification_source(session);
            if !source.is_empty()
                && let Ok(Some(previous)) = self.store.notification_source(&source, 0)
                && previous.kind == "job"
                && matches!(
                    previous.title.as_str(),
                    "Scheduled task failed"
                        | "Scheduled task paused"
                        | "Scheduled task interrupted"
                )
            {
                let _ = self.store.kv_set(&format!("notify_text:{source}"), "");
                let _ = self.send_notification(NewNotification {
                    title: if outcome == "ok" {
                        "Scheduled task recovered".into()
                    } else {
                        title.into()
                    },
                    body: plain_line(&name, 80),
                    from: "Scheduled tasks".into(),
                    session_id: Some(session.into()),
                    source_key: source,
                    kind: "job".into(),
                    quiet: true,
                    replace: Replace::Same,
                    ..Default::default()
                });
            }
            return;
        }
        let name = plain_line(&name, 80);
        let body = if detail.trim().is_empty() {
            name
        } else {
            format!("{name}: {}", plain_line(detail, 300))
        };
        let from = job
            .as_ref()
            .and_then(|j| j.bot_id.as_deref())
            .and_then(|b| self.find_bot(b))
            .map(|b| plain_line(&b.name, 40))
            .unwrap_or_else(|| "Scheduled tasks".into());
        if let Err(e) = self.send_notification(NewNotification {
            title: title.into(),
            body,
            from,
            session_id: Some(session.to_string()),
            due_at: now,
            kind: "job".into(),
            source_key: self.job_notification_source(session),
            urgent: true,
            quiet: !alert,
            replace: Replace::Same,
        }) {
            tracing::warn!("couldn't list how job run {session} ended: {e}");
        }
    }

    /// The job whose latest run is `session`, its status updated (runs
    /// started before runs were recorded).
    fn legacy_job_status(&self, session: &str, outcome: &str, now: u64) -> Option<Job> {
        let mut job = self
            .store
            .jobs()
            .ok()?
            .into_iter()
            .find(|j| j.last_session.as_deref() == Some(session))?;
        job.last_status = Some(outcome.into());
        job.last_end = Some(now);
        self.store.put_job(&job).ok()?;
        Some(job)
    }

    /// `job`'s schedule is over (`why`): it is off from now on, said once
    /// in the notification center.
    pub(super) fn end_schedule(&self, job: Job, why: &str) -> Job {
        let gate = self.job_gate(&job.id);
        let _admission = gate.lock();
        let Some(current) = self
            .store
            .jobs()
            .ok()
            .and_then(|v| v.into_iter().find(|j| j.id == job.id))
        else {
            return job;
        };
        // A stale scheduler/run cannot end a reconfigured/reassigned duty.
        if current.runs != job.runs
            || current.bot_id != job.bot_id
            || current.until != job.until
            || current.max_runs != job.max_runs
            || current.schedule != job.schedule
            || current.next_run != job.next_run
        {
            return current;
        }
        self.end_schedule_locked(current, why)
    }

    fn end_schedule_locked(&self, mut job: Job, why: &str) -> Job {
        job.enabled = false;
        job.next_run = None;
        job.ended = Some(why.to_string());
        if let Err(e) = self.store.put_job(&job) {
            tracing::error!("ending the schedule of job {}: {e}", job.id);
            return job;
        }
        self.sched.notify_one();
        let _ = self.events.send(Event::JobUpdated { job: job.clone() });
        let class = if job.max_runs.is_some_and(|m| job.runs >= m) {
            "max_runs"
        } else if job.until.is_some()
            && why != "a run said its goal was met"
            && !why.starts_with("its goal was met")
        {
            "until"
        } else {
            "goal_met"
        };
        tracing::info!(job_id = %job.id, outcome = class, "schedule ended");
        let _ = self.send_notification(NewNotification {
            title: "Scheduled task ended".into(),
            body: format!("{}: {why}.", plain_line(&job.name, 80)),
            from: "Scheduled tasks".into(),
            due_at: now_ms(),
            kind: "job".into(),
            quiet: true,
            ..Default::default()
        });
        job
    }

    /// `schedule end` from a job's run (or its helpers): its goal is met,
    /// no more runs.
    pub(super) fn end_own_schedule(&self, session: &str, reason: &str) -> Result<String, String> {
        let root = self.root_session(session);
        let run = self
            .store
            .job_run(&root)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                "Only a scheduled job's own run can end its schedule, and this chat isn't one. To stop a job from here, `update` it with `enabled: false`.".to_string()
            })?;
        let gate = self.job_gate(&run.job_id);
        let _admission = gate.lock();
        let job = self
            .store
            .jobs()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|j| j.id == run.job_id)
            .ok_or("This run's job was deleted already.")?;
        let owner = self
            .rt(&root)
            .map_err(|e| e.message)?
            .st
            .lock()
            .info
            .bot_id
            .clone();
        if job.bot_id.is_some() && job.bot_id != owner {
            return Err("This duty belongs to another bot now; only its current bot or the user can change it.".into());
        }
        if job.ended.is_some() && !job.enabled {
            return Ok(format!(
                "The schedule of “{}” had ended already: no more runs.",
                job.name
            ));
        }
        let reason = plain_line(reason, 200);
        let why = if reason.is_empty() {
            "a run said its goal was met".to_string()
        } else {
            format!("its goal was met ({reason})")
        };
        let job = self.end_schedule_locked(job, &why);
        Ok(format!(
            "Ended the schedule of “{}”: no more runs. Finish this run's report as usual.",
            job.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(engine: &Engine) -> Vec<UserNotification> {
        engine.store().notifications(500).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_for_the_user_is_listed_only_once_it_takes_a_while() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        // Answered at once: never listed.
        let v = engine
            .0
            .wait_for_user(&id, "Needs your approval", "rm -rf build", async { 7 })
            .await;
        assert_eq!(v, 7);
        assert!(items(&engine).is_empty());
        // Still waiting after a while: listed, one per chat.
        let me = engine.clone();
        let chat = id.clone();
        let wait = tokio::spawn(async move {
            me.0.wait_for_user(&chat, "Needs your approval", "rm -rf build", async {
                tokio::time::sleep(Duration::from_secs(45)).await;
            })
            .await
        });
        tokio::time::sleep(Duration::from_secs(40)).await;
        let listed = items(&engine);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(
            (
                listed[0].kind.as_str(),
                listed[0].title.as_str(),
                listed[0].body.as_str()
            ),
            ("approval", "Needs your approval", "rm -rf build")
        );
        assert!(!listed[0].quiet && !listed[0].seen);
        assert_eq!(listed[0].session_id.as_deref(), Some(id.as_str()));
        wait.await.unwrap();
        // Answered: read (gone, as it was only a moment ago on the clock).
        assert!(
            items(&engine).iter().all(|n| n.seen),
            "{:?}",
            items(&engine)
        );
    }

    #[tokio::test]
    async fn helpers_reports_are_counted_in_one_item_per_chat() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        engine.0.helper_reported(&id, "Reviewer", true);
        engine.0.helper_reported(&id, "Tester", false);
        let listed = items(&engine);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].title, "2 helpers reported back");
        assert_eq!(listed[0].body, "Reviewer, Tester (didn't finish)");
        // A job's helpers: its run reports for them.
        let job = engine
            .0
            .create_session(
                None,
                None,
                Some("Nightly".into()),
                SessionKind::Job,
                None,
                None,
                None,
            )
            .unwrap();
        let job_id = job.st.lock().info.id.clone();
        engine.0.helper_reported(&job_id, "Reviewer", true);
        assert_eq!(items(&engine).len(), 1);
    }

    #[tokio::test]
    async fn a_turn_that_ran_a_while_is_listed_when_it_ends() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let long_ago = std::time::Instant::now() - Duration::from_secs(90);
        let just_now = std::time::Instant::now();
        // Short: nothing. Handed on to the next queued message: nothing yet.
        engine
            .0
            .turn_settled(&rt, &Outcome::Done, false, (just_now, now_ms(), 0));
        engine
            .0
            .turn_settled(&rt, &Outcome::Done, true, (long_ago, now_ms(), 0));
        assert!(items(&engine).is_empty());
        engine
            .0
            .turn_settled(&rt, &Outcome::Aborted, false, (long_ago, now_ms(), 0));
        assert!(items(&engine).is_empty(), "the user stopped it");
        engine.0.turn_settled(
            &rt,
            &Outcome::Failed("the model server is down".into()),
            false,
            (long_ago, now_ms(), 0),
        );
        let listed = items(&engine);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].kind.as_str(), listed[0].title.as_str()),
            ("error", "Stopped by an error")
        );
        assert!(listed[0].body.ends_with("the model server is down"));
        // Its next end takes the unread one's place.
        engine
            .0
            .turn_settled(&rt, &Outcome::Done, false, (long_ago, now_ms(), 0));
        let listed = items(&engine);
        assert_eq!(
            listed.len(),
            2,
            "history retains the earlier read error: {listed:?}"
        );
        assert!(
            listed
                .iter()
                .any(|n| n.kind == "finished" && n.title == "Finished")
        );
    }

    #[tokio::test]
    async fn an_idle_job_waiting_for_delegated_work_is_not_over_or_ready_for_another_run() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        rt.st.lock().info.kind = SessionKind::Job;
        let job = Job {
            id: "job_waiting".into(),
            name: "Nightly".into(),
            last_session: Some(id.clone()),
            last_status: Some("running".into()),
            runs: 1,
            ..Default::default()
        };
        engine.store().put_job(&job).unwrap();
        engine.store().job_run_started(&job.id, &id, 1).unwrap();
        rt.st.lock().running = true;
        assert!(engine.0.job_is_working(&job));
        rt.st.lock().running = false;
        assert!(!engine.0.job_is_working(&job));
        let child = engine
            .0
            .create_session(
                None,
                None,
                Some("Worker".into()),
                SessionKind::Chat,
                None,
                None,
                None,
            )
            .unwrap();
        {
            let mut st = child.st.lock();
            st.running = true;
            st.info.working_for = Some(WorkingFor {
                session_id: id.clone(),
                call_id: "handoff".into(),
                since: now_ms(),
                from_seq: 0,
            });
        }
        assert!(engine.0.job_is_working(&job));
        let error = engine.0.clone().run_job(job.clone()).unwrap_err();
        assert!(error.to_string().contains("still working"));
        engine.0.job_turn_ended(&id, &Outcome::Done, 0);
        let run = engine.store().job_run(&id).unwrap().unwrap();
        assert_eq!(run.outcome, "running");
        assert!(run.ended_at.is_none());
        assert!(items(&engine).is_empty(), "no premature finished notice");
        child.st.lock().running = false;
        assert!(!engine.0.job_is_working(&job));
        engine.0.job_turn_ended(&id, &Outcome::Done, 0);
        assert_eq!(engine.store().job_run(&id).unwrap().unwrap().outcome, "ok");
    }

    #[tokio::test]
    async fn ending_an_old_run_cannot_disable_a_reassigned_bots_duty() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let sid = rt.st.lock().info.id.clone();
        rt.st.lock().info.bot_id = Some("bot_a".into());
        let mut job = Job {
            id: "job_reassigned".into(),
            bot_id: Some("bot_a".into()),
            enabled: true,
            ..Default::default()
        };
        engine.store().put_job(&job).unwrap();
        engine.store().job_run_started(&job.id, &sid, 1).unwrap();
        job.bot_id = Some("bot_b".into());
        engine.store().put_job(&job).unwrap();
        assert!(
            engine
                .0
                .end_own_schedule(&sid, "secret goal")
                .unwrap_err()
                .contains("another bot")
        );
        let saved = engine.store().jobs().unwrap().pop().unwrap();
        assert!(saved.enabled && saved.ended.is_none());
        rt.st.lock().info.bot_id = Some("bot_b".into());
        assert!(engine.0.end_own_schedule(&sid, "secret goal").is_ok());
        assert!(!engine.store().jobs().unwrap().pop().unwrap().enabled);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn job_admission_serializes_manual_manual_and_manual_scheduler_with_run_limits() {
        for scheduler in [false, true] {
            let (engine, dir, _rt) = super::super::test_support::engine().await;
            let job = Job {
                id: "job_race".into(),
                name: "Race".into(),
                prompt: "Do nothing".into(),
                cwd: dir.path().to_string_lossy().into(),
                schedule: "@every 1h".into(),
                enabled: true,
                max_runs: Some(1),
                next_run: Some(now_ms()),
                ..Default::default()
            };
            engine.store().put_job(&job).unwrap();
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let mut tasks = Vec::new();
            for scheduled in [false, scheduler] {
                let (inner, barrier, id) = (engine.0.clone(), barrier.clone(), job.id.clone());
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    inner.admit_job(&id, scheduled)
                }));
            }
            let a = tasks.remove(0).await.unwrap();
            let b = tasks.remove(0).await.unwrap();
            assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
            let saved = engine.store().jobs().unwrap().pop().unwrap();
            assert_eq!(saved.runs, 1);
            assert_eq!(engine.store().job_runs(&job.id).unwrap().len(), 1);
            assert_eq!(
                saved.last_session.as_deref(),
                Some(
                    engine.store().job_runs(&job.id).unwrap()[0]
                        .session_id
                        .as_str()
                )
            );
        }
    }

    #[tokio::test]
    async fn schedule_end_logs_only_identifiers_and_a_fixed_reason_class() {
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let sid = rt.st.lock().info.id.clone();
        let job = Job {
            id: "job_log_fixture".into(),
            name: "SENSITIVE_JOB_NAME".into(),
            prompt: "SENSITIVE_INSTRUCTIONS".into(),
            enabled: true,
            ..Default::default()
        };
        engine.store().put_job(&job).unwrap();
        engine.store().job_run_started(&job.id, &sid, 1).unwrap();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let capture = Capture(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            engine.0.end_own_schedule(&sid, "SENSITIVE_PRIVATE_REASON")
        })
        .unwrap();
        let text = String::from_utf8(bytes.lock().clone()).unwrap();
        assert!(
            text.contains("job_log_fixture") && text.contains("goal_met"),
            "{text}"
        );
        assert!(!text.contains("SENSITIVE_"), "{text}");
    }
}
