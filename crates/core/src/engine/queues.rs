//! Queue admission is durable before acknowledgement. Selected messages stay
//! in `delivering` until their history entry and queue removal commit together.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Snapshot {
    now: Vec<Waiting>,
    steer: Vec<Waiting>,
    followups: Vec<Waiting>,
    delivering: Vec<(Queue, Waiting)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_turn_stops: Option<StopMarks>,
}

impl Snapshot {
    fn of(st: &RtState) -> Self {
        Self {
            now: st.send_now.clone(),
            steer: st.steer.clone(),
            followups: st.followups.clone(),
            delivering: st.delivering.clone(),
            next_turn_stops: st.next_turn_stops.clone(),
        }
    }
    fn restore(self, st: &mut RtState) {
        st.send_now = self.now;
        st.steer = self.steer;
        st.followups = self.followups;
        st.delivering = self.delivering;
        st.next_turn_stops = self.next_turn_stops;
    }
}

impl RtState {
    pub(super) fn has_admitted(&self, q: Queue) -> bool {
        let list = match q {
            Queue::Now => &self.send_now,
            Queue::Steer => &self.steer,
            Queue::FollowUp => &self.followups,
        };
        list.iter().any(|w| w.stops == self.turn_stops)
    }

    pub(super) fn take_one(&mut self, q: Queue) -> Option<Waiting> {
        let marks = self.turn_stops.clone();
        self.take_admitted(q, &marks, true).pop()
    }
    pub(super) fn take_queue(&mut self, q: Queue) -> Vec<Waiting> {
        let marks = self.turn_stops.clone();
        self.take_admitted(q, &marks, false)
    }

    /// Automatic delivery never borrows another item's newer authority.
    /// A retained pre-Stop message needs an explicit user Send now.
    pub(super) fn take_admitted(&mut self, q: Queue, marks: &StopMarks, one: bool) -> Vec<Waiting> {
        let mut rows = Vec::new();
        let list = self.queue(q);
        let mut i = 0;
        while i < list.len() {
            if list[i].stops == *marks {
                rows.push(list.remove(i));
                if one {
                    break;
                }
            } else {
                i += 1;
            }
        }
        self.delivering.extend(rows.iter().cloned().map(|w| (q, w)));
        rows
    }
}

impl Inner {
    pub(super) fn restore_queue(&self, st: &mut RtState) -> Result<()> {
        if let Some(data) = self.store.queued(&st.info.id)? {
            let snapshot: Snapshot = serde_json::from_str(&data)?;
            snapshot.restore(st);
            // A crash between selecting a message and appending it leaves it
            // first in its original queue, never both queued and in history.
            let mut first = Snapshot::default();
            for (q, w) in std::mem::take(&mut st.delivering) {
                match q {
                    Queue::Now => first.now.push(w),
                    Queue::Steer => first.steer.push(w),
                    Queue::FollowUp => first.followups.push(w),
                }
            }
            st.send_now.splice(0..0, first.now);
            st.steer.splice(0..0, first.steer);
            st.followups.splice(0..0, first.followups);
        }
        Ok(())
    }

    pub(super) fn persist_queue(&self, st: &RtState) -> Result<()> {
        if st.info.private {
            return Ok(());
        }
        self.store
            .put_queued(&st.info.id, &serde_json::to_string(&Snapshot::of(st))?)
    }

    /// Rejected writes restore admission state instead of executing work the
    /// sender was told did not enter the queue.
    pub(super) fn update_queue<T>(
        &self,
        st: &mut RtState,
        edit: impl FnOnce(&mut RtState) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let before = Snapshot::of(st);
        let running = st.running;
        let result =
            edit(st).and_then(|value| self.persist_queue(st).map(|()| value).map_err(internal));
        if result.is_err() {
            before.restore(st);
            st.running = running;
        } else if st.send_now.iter().any(|w| {
            !before
                .now
                .iter()
                .any(|old| old.id == w.id && old.stops == w.stops)
        }) {
            // A new item or explicit re-admission can interrupt only after
            // persistence commits, even if the queue's length didn't grow.
            if let Some(cancel) = &st.cancel {
                cancel.cancel();
            }
        }
        result
    }

    pub(super) fn append_waiting(&self, rt: &Rt, w: Waiting) -> Result<Entry> {
        let (id, entry) = {
            let mut st = rt.st.lock();
            let before = Snapshot::of(&st);
            st.delivering.retain(|(_, pending)| pending.id != w.id);
            let data = serde_json::to_string(&Snapshot::of(&st))?;
            let id = st.info.id.clone();
            let entry = match self.store.append_with_queue(
                &id,
                EntryBody::User {
                    content: w.content,
                    author: w.author,
                },
                Some(&data),
            ) {
                Ok(entry) => entry,
                Err(e) => {
                    before.restore(&mut st);
                    return Err(e);
                }
            };
            (id, entry)
        };
        self.record_entry(rt, id, entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_send_now_admission_does_not_cancel_running_work() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let mut st = rt.st.lock();
        st.running = true;
        let cancel = CancellationToken::new();
        st.cancel = Some(cancel.clone());
        engine.0.store.delete_session(&st.info.id).unwrap();
        assert!(
            engine
                .0
                .update_queue(&mut st, |st| {
                    st.next_turn_stops = Some(StopMarks {
                        here: 9,
                        ..Default::default()
                    });
                    Ok(interrupt(
                        st,
                        Waiting::user(vec![ContentPart::text("rejected")], StopMarks::default()),
                    ))
                })
                .is_err()
        );
        assert!(!cancel.is_cancelled());
        assert!(st.send_now.is_empty());
        assert!(st.next_turn_stops.is_none());
        engine.0.store.put_session(&st.info, &st.internal).unwrap();
        engine
            .0
            .update_queue(&mut st, |st| {
                Ok(interrupt(
                    st,
                    Waiting::user(vec![ContentPart::text("accepted")], StopMarks::default()),
                ))
            })
            .unwrap();
        assert!(cancel.is_cancelled());
        assert_eq!(st.send_now.len(), 1);
    }

    #[tokio::test]
    async fn acknowledged_queue_edits_survive_restart() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        rt.st.lock().running = true;
        rt.st.lock().cancel = Some(CancellationToken::new());
        for (text, delivery) in [
            ("follow", Delivery::Auto),
            ("steer", Delivery::Steer),
            ("now", Delivery::Interrupt),
        ] {
            engine
                .0
                .send(&id, vec![ContentPart::text(text)], delivery, Vec::new())
                .unwrap();
        }
        engine
            .handle(
                Request::EditQueued {
                    session_id: id.clone(),
                    index: 2,
                    text: "edited".into(),
                },
                None,
            )
            .await
            .unwrap();
        let restarted = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let restored = restarted.0.rt(&id).unwrap();
        assert_eq!(
            restored.st.lock().next_turn_stops,
            rt.st.lock().next_turn_stops
        );
        assert_eq!(
            queued_messages(&restored.st.lock())
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            ["now", "steer", "edited"]
        );
        restarted
            .handle(
                Request::Dequeue {
                    session_id: id.clone(),
                    index: 1,
                },
                None,
            )
            .await
            .unwrap();
        let twice = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        assert_eq!(
            queued_messages(&twice.0.rt(&id).unwrap().st.lock())
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            ["now", "edited"]
        );
    }

    #[tokio::test]
    async fn queued_payload_modes_order_and_selection_survive_restart() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        {
            let mut st = rt.st.lock();
            for (q, text) in [
                (Queue::FollowUp, "first"),
                (Queue::FollowUp, "second"),
                (Queue::Steer, "steer"),
                (Queue::Now, "now"),
            ] {
                st.queue(q).push(Waiting::user(
                    vec![ContentPart::text(text)],
                    StopMarks::default(),
                ));
            }
            engine.0.persist_queue(&st).unwrap();
            // Selected, but not committed to history: a crash must replay it.
            st.take_one(Queue::FollowUp).unwrap();
            engine.0.persist_queue(&st).unwrap();
        }
        let restarted = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let restored = restarted.0.rt(&id).unwrap();
        let messages = queued_messages(&restored.st.lock());
        assert_eq!(
            messages.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["now", "steer", "first", "second"]
        );
        assert!(messages[0].interrupt && messages[1].steer);
        let first = restored.st.lock().take_one(Queue::FollowUp).unwrap();
        restarted.0.append_waiting(&restored, first).unwrap();
        let twice = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let restored = twice.0.rt(&id).unwrap();
        assert_eq!(
            queued_messages(&restored.st.lock())
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>(),
            ["now", "steer", "second"]
        );
        let entries = twice.0.store.entries(&id, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(&entries[0].body, EntryBody::User { content, .. } if plain_text(content) == "first")
        );
    }

    #[tokio::test]
    async fn queue_write_failure_is_not_acknowledged_and_private_queue_is_memory_only() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let mut st = rt.st.lock();
        st.running = true;
        engine.0.store.delete_session(&st.info.id).unwrap();
        assert!(
            engine
                .0
                .update_queue(&mut st, |st| {
                    st.followups.push(Waiting::user(
                        vec![ContentPart::text("rejected")],
                        StopMarks::default(),
                    ));
                    Ok(())
                })
                .is_err()
        );
        assert!(st.followups.is_empty());
        st.info.private = true;
        engine
            .0
            .update_queue(&mut st, |st| {
                st.followups.push(Waiting::user(
                    vec![ContentPart::text("memory only")],
                    StopMarks::default(),
                ));
                Ok(())
            })
            .unwrap();
        assert!(engine.0.store.queued(&st.info.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn every_queue_and_delivering_item_persists_admission_marks() {
        let (engine, dir, rt) = super::super::test_support::engine().await;
        let marks = StopMarks {
            here: 7,
            machines: [
                ("local-node".to_string(), 9),
                ("remote-node".to_string(), 2),
            ]
            .into(),
        };
        let id = {
            let mut st = rt.st.lock();
            st.turn_stops = marks.clone();
            for (q, text) in [
                (Queue::Now, "now"),
                (Queue::Steer, "steer"),
                (Queue::FollowUp, "follow"),
            ] {
                st.queue(q)
                    .push(Waiting::user(vec![ContentPart::text(text)], marks.clone()));
            }
            st.take_one(Queue::FollowUp).unwrap();
            engine.0.persist_queue(&st).unwrap();
            st.info.id.clone()
        };
        let restarted = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let restored = restarted.0.rt(&id).unwrap();
        let st = restored.st.lock();
        for item in st.send_now.iter().chain(&st.steer).chain(&st.followups) {
            assert_eq!(item.stops, marks);
        }
        assert_eq!(queued_messages(&st).len(), 3);
        assert!(
            st.delivering.is_empty(),
            "interrupted delivery restores its original queue and marks"
        );
    }

    #[tokio::test]
    async fn failed_readmission_rolls_back_per_item_marks() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let old = StopMarks {
            here: 3,
            ..Default::default()
        };
        let newer = StopMarks {
            here: 4,
            ..Default::default()
        };
        let mut st = rt.st.lock();
        st.running = true;
        let cancel = CancellationToken::new();
        st.cancel = Some(cancel.clone());
        st.followups.push(Waiting::user(
            vec![ContentPart::text("queued")],
            old.clone(),
        ));
        engine.0.store.delete_session(&st.info.id).unwrap();
        assert!(
            engine
                .0
                .update_queue(&mut st, |st| {
                    let mut w = st.followups.remove(0);
                    w.stops = newer;
                    Ok(interrupt(st, w))
                })
                .is_err()
        );
        assert_eq!(st.followups.len(), 1);
        assert_eq!(st.followups[0].stops, old);
        assert!(st.send_now.is_empty());
        assert!(!cancel.is_cancelled());
    }

    #[tokio::test]
    async fn legacy_queue_items_never_get_current_stop_marks() {
        let (_engine, _dir, rt) = super::super::test_support::engine().await;
        let legacy: Waiting = serde_json::from_value(json!({
            "id": "old-item", "content": [ContentPart::text("legacy work")], "author": null
        }))
        .unwrap();
        assert_eq!(legacy.stops, StopMarks::default());
        let marks = StopMarks {
            here: 4,
            ..Default::default()
        };
        let here = HereNow {
            stopped: false,
            file: 4,
            machines: vec![],
        };
        let mut st = rt.st.lock();
        st.turn_stops = marks;
        st.followups.push(legacy);
        assert!(st.take_one(Queue::FollowUp).is_none());
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here)
                .is_empty()
        );
        assert_eq!(st.followups.len(), 1);
        assert!(st.next_turn_stops.is_none());
    }

    #[tokio::test]
    async fn explicit_queued_send_now_readmits_only_the_selected_item() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let old = engine.0.stops_here();
        let id = {
            let mut st = rt.st.lock();
            st.running = true;
            st.cancel = Some(CancellationToken::new());
            st.send_now.push(Waiting::user(
                vec![ContentPart::text("selected old now")],
                old.clone(),
            ));
            st.followups.push(Waiting::user(
                vec![ContentPart::text("other old work")],
                old.clone(),
            ));
            st.info.id.clone()
        };
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let fresh = engine.0.stops_here();
        engine
            .handle(
                Request::InterruptQueued {
                    session_id: id,
                    index: 0,
                },
                None,
            )
            .await
            .unwrap();
        let st = rt.st.lock();
        assert_eq!(st.send_now.len(), 1);
        assert_eq!(st.send_now[0].stops, fresh);
        assert_eq!(st.followups[0].stops, old);
        assert!(st.cancel.as_ref().unwrap().is_cancelled());
    }
}
