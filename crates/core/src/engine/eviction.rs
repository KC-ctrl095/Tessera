//! A small idle runtime cache; persistence, not the cache, owns a chat.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const IDLE_LIMIT: usize = 128;
const IDLE_MS: u64 = 5 * 60 * 1000;

impl Inner {
    pub(super) fn load_gate(&self, id: &str) -> Arc<Mutex<()>> {
        let mut loads = self.loading.lock();
        loads.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = loads.get(id).and_then(std::sync::Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(Mutex::new(()));
        loads.insert(id.to_string(), Arc::downgrade(&gate));
        gate
    }

    pub(super) fn maintain_sessions(&self) {
        let now = now_ms();
        let next = self.next_eviction.load(Ordering::Relaxed);
        if now >= next
            && self
                .next_eviction
                .compare_exchange(next, now + 60_000, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.evict_idle(IDLE_LIMIT, now.saturating_sub(IDLE_MS));
        }
    }

    pub(super) fn evict_idle(&self, limit: usize, idle_before: u64) -> usize {
        // Admission paths can already hold one of these locks. Never wait for
        // it during maintenance, and never follow map -> state lock ordering.
        let Some(background) = self.background.try_lock() else {
            return 0;
        };
        let mut pinned = HashSet::new();
        for (parent, agents) in background.iter() {
            if agents.iter().any(|a| !a.done) {
                pinned.insert(parent.clone());
            }
            pinned.extend(
                agents
                    .iter()
                    .filter(|a| !a.done)
                    .map(|a| a.session_id.clone()),
            );
        }
        drop(background);
        let Some(approvals) = self.approvals.try_lock() else {
            return 0;
        };
        pinned.extend(approvals.values().map(|p| p.shown_on.clone()));
        drop(approvals);
        let mut candidates: Vec<_> = self
            .sessions
            .lock()
            .iter()
            .map(|(id, rt)| (id.clone(), rt.clone()))
            .collect();
        candidates.sort_by_key(|(_, rt)| rt.used.load(Ordering::Relaxed));
        let mut removed = 0;
        for (id, rt) in candidates {
            if pinned.contains(&id) {
                continue;
            }
            let Some(st) = rt.st.try_lock() else {
                continue;
            };
            if st.running || st.cancel.is_some() || st.streaming.is_some()
                || !st.steer.is_empty() || !st.followups.is_empty() || !st.send_now.is_empty() || !st.delivering.is_empty()
                || st.info.private || st.info.kind == SessionKind::Subagent || st.info.working_for.is_some()
                || matches!(st.info.status, SessionStatus::Running | SessionStatus::AwaitingApproval | SessionStatus::Compacting)
                // A child reads this exact ceiling; don't replace its owner
                // with a reloaded runtime until all children let it go.
                || Arc::strong_count(&st.permission_ceiling) > 1
                // Session-local tool ceilings outside persisted helper specs
                // cannot be reconstructed safely, so keep those runtimes.
                || (st.tools_only.is_some() && st.internal.subagent.is_none())
            {
                continue;
            }
            let mut map = self.sessions.lock();
            let old = rt.used.load(Ordering::Relaxed) <= idle_before;
            if (!old && map.len() <= limit)
                || Arc::strong_count(&rt) != 2
                || !map
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, &rt))
            {
                continue;
            }
            // Clean per-session resources before allowing a fresh lookup;
            // otherwise cleanup could revoke a new runtime's resources.
            self.files.forget_session(&id);
            if let Some(lsp) = self.lsp.read().as_ref() {
                lsp.forget_session(&id);
            }
            self.claude.close_for(&id);
            map.remove(&id);
            drop(map);
            drop(st);
            removed += 1;
        }
        removed
    }
}

pub(super) fn used_now() -> AtomicU64 {
    AtomicU64::new(now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn an_expired_card_rejects_a_reply_before_the_waiter_gets_scheduled() {
        for decision in [
            ApprovalDecision::Allow,
            ApprovalDecision::AllowSession,
            ApprovalDecision::Deny,
        ] {
            let (engine, _dir, rt) = super::super::test_support::engine().await;
            let id = rt.st.lock().info.id.clone();
            let (tx, rx) = oneshot::channel();
            engine.0.approvals.lock().insert(
                "late".into(),
                Pending {
                    shown_on: id.clone(),
                    acting_on: id.clone(),
                    deadline: tokio::time::Instant::now() + approval::WAIT,
                    tx,
                    authority: None,
                    request: ApprovalRequest {
                        id: "late".into(),
                        call_id: "fixture".into(),
                        tool: "bash".into(),
                        summary: "fixture".into(),
                        reason: "fixture".into(),
                        arguments: json!({}),
                        once: false,
                    },
                },
            );
            tokio::time::advance(approval::WAIT).await;
            let result = engine
                .handle(
                    Request::RespondApproval {
                        session_id: id,
                        request_id: "late".into(),
                        decision,
                    },
                    None,
                )
                .await;
            assert!(result.is_err());
            assert_eq!(rx.await.unwrap().outcome, ApprovalOutcome::Expired);
            assert!(!engine.0.approvals.lock().contains_key("late"));
        }
    }
    #[tokio::test]
    async fn idle_runtime_cache_pressure_is_bounded() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        drop(rt);
        for _ in 0..IDLE_LIMIT + 1 {
            engine
                .0
                .create_session(None, None, None, SessionKind::Chat, None, None, None)
                .unwrap();
        }
        assert_eq!(engine.0.sessions.lock().len(), IDLE_LIMIT + 2);
        assert_eq!(engine.0.evict_idle(IDLE_LIMIT, 0), 2);
        assert_eq!(engine.0.sessions.lock().len(), IDLE_LIMIT);
    }

    #[tokio::test]
    async fn concurrent_loaders_do_not_publish_pre_eviction_permissions() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        drop(rt);
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 1);
        let gate = engine.0.load_gate(&id);
        assert!(Arc::ptr_eq(&gate, &engine.0.load_gate(&id)));
        let loading = gate.lock();
        let inner = engine.0.clone();
        let sid = id.clone();
        let (entered, wait) = std::sync::mpsc::channel();
        let loader = std::thread::spawn(move || {
            entered.send(()).unwrap();
            inner.rt(&sid).unwrap().st.lock().info.permission
        });
        wait.recv().unwrap();
        // The loader is fenced before it can read stale Auto state, even if
        // another runtime was downgraded and evicted while it waited.
        let (mut info, internal) = engine.0.store.get_session(&id).unwrap().unwrap();
        info.permission = PermissionMode::ReadOnly;
        engine.0.store.put_session(&info, &internal).unwrap();
        drop(loading);
        assert_eq!(loader.join().unwrap(), PermissionMode::ReadOnly);
        drop(gate);
        engine.0.load_gate("another");
        assert!(
            engine.0.loading.lock().len() <= 1,
            "finished load gates do not accumulate"
        );
    }

    #[tokio::test]
    async fn inactive_runtimes_are_evicted_and_reload_persisted_state() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        rt.st.lock().internal.system_prompt = Some("Kept prompt".into());
        engine.0.save(&rt);
        assert_eq!(
            engine.0.evict_idle(0, u64::MAX),
            0,
            "held Arc pins work and readers"
        );
        drop(rt);
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 1);
        assert!(engine.0.live_session(&id).is_none());
        let reloaded = engine.0.rt(&id).unwrap();
        assert_eq!(
            reloaded.st.lock().internal.system_prompt.as_deref(),
            Some("Kept prompt")
        );
        assert_eq!(reloaded.st.lock().info.id, id);
    }

    #[tokio::test]
    async fn running_queued_approval_and_report_dependencies_pin_runtimes() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let id = rt.st.lock().info.id.clone();
        rt.st.lock().running = true;
        drop(rt);
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 0);
        {
            let rt = engine.0.rt(&id).unwrap();
            let mut st = rt.st.lock();
            st.running = false;
            st.followups.push(Waiting::user(
                vec![ContentPart::text("waiting")],
                StopMarks::default(),
            ));
        }
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 0);
        engine.0.rt(&id).unwrap().st.lock().followups.clear();
        let (tx, _rx) = oneshot::channel();
        engine.0.approvals.lock().insert(
            "approval".into(),
            Pending {
                shown_on: id.clone(),
                acting_on: id.clone(),
                deadline: tokio::time::Instant::now() + approval::WAIT,
                tx,
                authority: None,
                request: ApprovalRequest {
                    id: "approval".into(),
                    call_id: "call".into(),
                    tool: "fixture".into(),
                    summary: "Fixture".into(),
                    reason: "Fixture".into(),
                    arguments: json!({}),
                    once: false,
                },
            },
        );
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 0);
        engine.0.approvals.lock().clear();
        engine.0.background.lock().insert(
            id.clone(),
            vec![BgAgent {
                name: "helper".into(),
                session_id: "helper".into(),
                started: std::time::Instant::now(),
                cancel: CancellationToken::new(),
                batch: 0,
                done: false,
                stopped: false,
            }],
        );
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 0);
        engine.0.background.lock().clear();
        assert_eq!(engine.0.evict_idle(0, u64::MAX), 1);
    }
}
