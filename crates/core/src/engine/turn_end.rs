//! Finalization consumes only admitted work whose Stop provenance is still
//! valid. Stop and failures leave earlier work queued for the user.
use super::*;

impl RtState {
    pub(super) fn finish_admission(
        &mut self,
        outcome: &Outcome,
        status: SessionStatus,
        here: &HereNow,
    ) -> Vec<Waiting> {
        // Stop may win admission after the model loop returned Done. Its
        // cancellation must override that stale outcome, but not an explicit
        // send-now handoff (which Stop moves back to followups).
        let cancelled = self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        // The one-second watcher need not have cancelled the token yet.
        // Read-before-lock snapshot, not Done, decides whether this turn
        // can automatically consume the work that arrived while it ended.
        let ending_live = !here.stopped && here.since(&self.turn_stops);
        let done = matches!(outcome, Outcome::Done) && !cancelled && ending_live;
        self.cancel = None;
        self.streaming = None;
        // A failed history write is still accepted work, not a delivered entry.
        let pending = std::mem::take(&mut self.delivering);
        for (q, w) in pending.into_iter().rev() {
            self.queue(q).insert(0, w);
        }
        let eligible = |w: &&Waiting| !here.stopped && here.since(&w.stops);
        let marks = self
            .send_now
            .iter()
            .find(eligible)
            .or_else(|| {
                done.then(|| self.steer.iter().chain(&self.followups).find(eligible))
                    .flatten()
            })
            .map(|w| w.stops.clone());
        let mut next = Vec::new();
        if let Some(marks) = &marks {
            next.extend(self.take_admitted(Queue::Now, marks, false));
            if done {
                next.extend(self.take_admitted(Queue::Steer, marks, false));
                next.extend(self.take_admitted(Queue::FollowUp, marks, true));
            }
        }
        if (matches!(outcome, Outcome::Aborted) || cancelled || !ending_live) && next.is_empty() {
            let steer = std::mem::take(&mut self.steer);
            self.followups.splice(0..0, steer);
        }
        self.running = !next.is_empty();
        self.info.status = if self.running {
            SessionStatus::Running
        } else {
            status
        };
        // Only the selected items' recorded admission can authorize the
        // next turn. Never mint fresh marks at finalization or mix epochs.
        self.next_turn_stops = (!next.is_empty()).then_some(marks).flatten();
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here(marks: &StopMarks) -> HereNow {
        HereNow {
            stopped: false,
            file: marks.here,
            machines: marks
                .machines
                .iter()
                .map(|(id, n)| (id.clone(), *n))
                .collect(),
        }
    }

    fn waiting(text: &str, stops: &StopMarks) -> Waiting {
        Waiting::user(vec![ContentPart::text(text)], stops.clone())
    }

    #[tokio::test]
    async fn finalization_consumes_late_followups_reports_and_bot_replies() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        // The last model-loop queue check already saw nothing. Each source
        // can still race in before the one atomic admission/idle transition.
        for author in [None, Some("subagent:fixture"), Some("bot:fixture")] {
            let here = engine.0.here_now();
            let mut st = rt.st.lock();
            st.running = true;
            assert!(st.steer.is_empty() && st.followups.is_empty());
            let mut w = waiting("late work", &st.turn_stops);
            w.author = author.map(str::to_string);
            st.queue(if author.is_some() {
                Queue::Steer
            } else {
                Queue::FollowUp
            })
            .push(w);
            engine.0.persist_queue(&st).unwrap();
            let next = st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here);
            assert_eq!(next.len(), 1);
            assert!(st.running);
            assert_eq!(st.info.status, SessionStatus::Running);
            drop(st);
            engine.0.open_turn(&rt, next);
        }
        let id = rt.st.lock().info.id.clone();
        assert_eq!(engine.0.store.entries(&id, 0).unwrap().len(), 3);
        let here = engine.0.here_now();
        assert!(
            rt.st
                .lock()
                .finish_admission(&Outcome::Done, SessionStatus::Idle, &here)
                .is_empty()
        );
        engine
            .0
            .send(
                &id,
                vec![ContentPart::text("after idle")],
                Delivery::Auto,
                vec![],
            )
            .unwrap();
        assert!(rt.st.lock().running);
    }

    #[tokio::test]
    async fn cancellation_wins_even_after_the_loop_returned_done() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let marks = StopMarks::default();
        let mut st = rt.st.lock();
        st.running = true;
        let cancel = CancellationToken::new();
        cancel.cancel();
        st.cancel = Some(cancel.clone());
        st.followups.push(waiting("retained followup", &marks));
        st.steer.push(waiting("retained steer", &marks));
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here(&marks))
                .is_empty()
        );
        assert!(!st.running);
        assert_eq!(queued_messages(&st).len(), 2);
        assert!(st.steer.is_empty());
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn send_now_cancellation_is_a_handoff_not_stranded_work() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let marks = StopMarks::default();
        let mut st = rt.st.lock();
        st.running = true;
        let cancel = CancellationToken::new();
        cancel.cancel();
        st.cancel = Some(cancel);
        st.send_now.push(waiting("send now", &marks));
        let next = st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here(&marks));
        assert_eq!(next.len(), 1);
        assert_eq!(plain_text(&next[0].content), "send now");
        assert_eq!(st.next_turn_stops, Some(marks));
        assert!(st.running);
    }

    #[tokio::test]
    async fn stop_and_failure_retain_accepted_work() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let marks = StopMarks::default();
        for outcome in [Outcome::Aborted, Outcome::Failed("fixture".into())] {
            let mut st = rt.st.lock();
            st.running = true;
            st.steer.clear();
            st.followups.clear();
            st.steer.push(waiting("steer", &marks));
            st.followups.push(waiting("follow", &marks));
            let next = st.finish_admission(&outcome, SessionStatus::Idle, &here(&marks));
            assert!(next.is_empty());
            assert!(!st.running);
            assert_eq!(queued_messages(&st).len(), 2);
        }
    }

    #[tokio::test]
    async fn finalization_keeps_report_and_send_now_admission_marks() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let mut st = rt.st.lock();
        let old = StopMarks {
            here: 3,
            ..Default::default()
        };
        let accepted = StopMarks {
            here: 4,
            ..Default::default()
        };
        st.turn_stops = old.clone();
        let mut report = waiting("late report", &old);
        report.author = Some("subagent:fixture".into());
        st.steer.push(report);
        st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here(&old));
        assert_eq!(st.next_turn_stops, Some(old.clone()));
        st.delivering.clear();
        st.next_turn_stops = Some(StopMarks {
            here: 99,
            ..Default::default()
        });
        st.send_now.push(waiting("send now", &accepted));
        st.finish_admission(&Outcome::Aborted, SessionStatus::Idle, &here(&accepted));
        assert_eq!(st.next_turn_stops, Some(accepted));
        st.delivering.clear();
        st.next_turn_stops = None;
        st.followups.push(waiting("pre-Stop user work", &old));
        let live = StopMarks {
            here: 5,
            ..Default::default()
        };
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here(&live))
                .is_empty()
        );
        assert!(st.next_turn_stops.is_none());
        assert_eq!(st.followups[0].stops, old);
    }

    #[tokio::test]
    async fn done_late_followup_stop_resume_does_not_wait_for_watcher() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        crate::node::StopLatch::set(&engine.0.paths, false, 3).unwrap();
        let admitted = engine.0.stops_here();
        let cancel = CancellationToken::new();
        let id = {
            let mut st = rt.st.lock();
            st.running = true;
            st.turn_stops = admitted.clone();
            st.cancel = Some(cancel.clone());
            st.info.id.clone()
        };
        // Done has already returned, but no finalization or watcher tick yet.
        let outcome = Outcome::Done;
        engine
            .0
            .send(
                &id,
                vec![ContentPart::text("late pre-Stop followup")],
                Delivery::Auto,
                vec![],
            )
            .unwrap();
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let here = engine.0.here_now(); // BEFORE the runtime lock, no await.
        assert!(!here.stopped);
        assert!(!here.since(&admitted));
        assert!(
            !cancel.is_cancelled(),
            "the regression must not rely on the watcher"
        );
        let mut st = rt.st.lock();
        assert!(
            st.finish_admission(&outcome, SessionStatus::Idle, &here)
                .is_empty()
        );
        assert!(!st.running);
        assert!(st.next_turn_stops.is_none());
        assert_eq!(st.followups.len(), 1);
        assert_eq!(st.followups[0].stops, admitted);
        assert_eq!(queued_messages(&st)[0].text, "late pre-Stop followup");
        engine.0.persist_queue(&st).unwrap();
    }

    #[tokio::test]
    async fn stopped_snapshot_refuses_done_even_with_equal_marks() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let marks = StopMarks {
            here: 3,
            ..Default::default()
        };
        let mut stopped = here(&marks);
        stopped.stopped = true;
        let mut st = rt.st.lock();
        st.running = true;
        st.turn_stops = marks.clone();
        st.followups.push(waiting("work", &marks));
        st.send_now.push(waiting("send now", &marks));
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &stopped)
                .is_empty()
        );
        assert_eq!(queued_messages(&st).len(), 2);
        assert!(st.next_turn_stops.is_none());
    }

    #[tokio::test]
    async fn fresh_send_now_cannot_reauthorize_older_items_or_mix_marks() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let old = engine.0.stops_here();
        let id = {
            let mut st = rt.st.lock();
            st.running = true;
            st.cancel = Some(CancellationToken::new());
            st.send_now.push(waiting("old send now", &old));
            st.steer.push(waiting("old steer", &old));
            st.followups.push(waiting("old followup", &old));
            st.info.id.clone()
        };
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let fresh = engine.0.stops_here();
        engine
            .0
            .send(
                &id,
                vec![ContentPart::text("fresh send now")],
                Delivery::Interrupt,
                vec![],
            )
            .unwrap();
        let here = engine.0.here_now();
        let mut st = rt.st.lock();
        let next = st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here);
        assert_eq!(next.len(), 1);
        assert_eq!(plain_text(&next[0].content), "fresh send now");
        assert_eq!(next[0].stops, fresh);
        assert_eq!(st.next_turn_stops, Some(fresh.clone()));
        assert_eq!(queued_messages(&st).len(), 3);
        st.delivering.clear();
        st.turn_stops = fresh;
        // A later fresh turn cannot drain retained earlier work internally.
        assert!(st.take_queue(Queue::Now).is_empty());
        assert!(st.take_queue(Queue::Steer).is_empty());
        assert!(st.take_one(Queue::FollowUp).is_none());
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here)
                .is_empty()
        );
        assert_eq!(queued_messages(&st).len(), 3);
    }

    #[tokio::test]
    async fn post_resume_followup_keeps_own_marks_across_restart() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let old = engine.0.stops_here();
        let id = {
            let mut st = rt.st.lock();
            st.running = true;
            st.cancel = Some(CancellationToken::new());
            st.info.id.clone()
        };
        engine
            .0
            .send(
                &id,
                vec![ContentPart::text("old followup")],
                Delivery::Auto,
                vec![],
            )
            .unwrap();
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let fresh = engine.0.stops_here();
        engine
            .0
            .send(
                &id,
                vec![ContentPart::text("fresh followup")],
                Delivery::Auto,
                vec![],
            )
            .unwrap();
        let here = engine.0.here_now();
        {
            let mut st = rt.st.lock();
            assert!(!st.cancel.as_ref().unwrap().is_cancelled());
            assert!(
                st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here)
                    .is_empty()
            );
            engine.0.persist_queue(&st).unwrap();
        }
        let restarted = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let restored = restarted.0.rt(&id).unwrap();
        let here = restarted.0.here_now();
        let mut st = restored.st.lock();
        assert!(!st.running);
        assert_eq!(st.followups[0].stops, old);
        assert_eq!(st.followups[1].stops, fresh);
        // A properly admitted newer turn may consume only the fresh item.
        st.running = true;
        st.turn_stops = fresh.clone();
        let next = st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here);
        assert_eq!(next.len(), 1);
        assert_eq!(plain_text(&next[0].content), "fresh followup");
        assert_eq!(st.next_turn_stops, Some(fresh));
        assert_eq!(st.followups.len(), 1);
        assert_eq!(st.followups[0].stops, old);
    }
}
