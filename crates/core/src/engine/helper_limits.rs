//! How many helpers (subagents) work at once, and for how long.
//!
//! Each chat chooses its own limits (`set_chat_helpers`, kept in
//! `SessionInfo::helper_limits`): helpers working at once, and the steps and
//! minutes each one gets. Any number, or no limit; what a chat leaves unset
//! follows the server's defaults (`[agent] max_subagents`, `subagent_steps`,
//! `subagent_minutes`). Whatever the chats choose, the server's capacity
//! protects the machine: at most that many helpers work at once across all
//! chats (from its memory and processors, or `[agent] helper_capacity`), and
//! no new one starts while memory runs low. A model
//! that answers one request at a time keeps its own lane (`Inner::lanes`).
//! Helpers past any of these wait in order and show as queued
//! (`SessionInfo::helper_queued`); nothing working is ever stopped by a
//! lower limit.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, OwnedSemaphorePermit};

use super::*;
use crate::tools::admission::ActionAdmission;

/// The wire's largest whole number; 0 selects no limit.
pub(crate) const MAX_LIMIT: u32 = u32::MAX;

/// "No limit" as a step or minute budget.
pub(crate) const UNLIMITED: u32 = u32::MAX;
pub(super) const STEP_EXHAUSTED: &str =
    "Budget exhausted; not executed. Write your final report without more tools.";

/// Owns cleanup before release. A successful allow commits its claim; a
/// later permission or MCP stage shares it. Only never-released claims
/// are returned by cancellation or restart.
pub(super) struct Provisional {
    inner: Arc<Inner>,
    rt: Arc<Rt>,
    call: String,
    armed: bool,
}
impl Provisional {
    pub(super) fn new(inner: &Arc<Inner>, rt: &Arc<Rt>, call: &str) -> Self {
        Self {
            inner: inner.clone(),
            rt: rt.clone(),
            call: call.into(),
            armed: true,
        }
    }
    pub(super) fn release_to(
        &mut self,
        queue: impl FnOnce() -> bool,
    ) -> Result<bool, &'static str> {
        let queued = self
            .inner
            .queue_helper_release(&self.rt, &self.call, queue)?;
        if queued {
            self.transfer();
        }
        Ok(queued)
    }
    pub(super) fn cancel_unreleased(&self) {
        self.inner.rollback_helper_step(&self.rt, &self.call);
    }
    pub(super) fn transfer(&mut self) {
        self.armed = false;
    }
}
impl Drop for Provisional {
    fn drop(&mut self) {
        if self.armed {
            self.inner.rollback_helper_step(&self.rt, &self.call);
        }
    }
}

pub(super) type StepCall = crate::agents::HelperStep;

/// A helper kept from starting by low memory looks again this often.
const RECHECK: Duration = Duration::from_secs(2);

/// First come, first served: `limit` helpers pass (none: any number), the
/// others wait in line. A new limit applies at once: more pass when it grows;
/// when it shrinks, those working finish and the next waits for the count
/// to drop below it.
pub(crate) struct Gate {
    state: Mutex<GateState>,
    changed: Notify,
}

struct GateState {
    limit: Option<usize>,
    working: usize,
    waiting: VecDeque<u64>,
    next: u64,
}

impl GateState {
    fn turn_of(&self, ticket: u64) -> bool {
        self.waiting.front() == Some(&ticket) && self.limit.is_none_or(|l| self.working < l)
    }
}

/// One helper's place among those working; gives it back when dropped.
pub(crate) struct Pass(Arc<Gate>);

impl Drop for Pass {
    fn drop(&mut self) {
        self.0.state.lock().working -= 1;
        self.0.changed.notify_waiters();
    }
}

/// A place in line; leaving (Stop) before its turn gives the place up.
struct Ticket<'a> {
    gate: &'a Gate,
    id: u64,
    admitted: bool,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.admitted {
            self.gate.state.lock().waiting.retain(|t| *t != self.id);
            self.gate.changed.notify_waiters();
        }
    }
}

enum Turn {
    In(Pass),
    /// Someone is ahead, or the limit is reached.
    Wait,
    /// Its turn, but `ready` said no (memory).
    NotReady,
}

impl Gate {
    pub(crate) fn new(limit: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState {
                limit,
                working: 0,
                waiting: VecDeque::new(),
                next: 0,
            }),
            changed: Notify::new(),
        })
    }

    pub(crate) fn set_limit(&self, limit: Option<usize>) {
        let mut s = self.state.lock();
        if s.limit != limit {
            s.limit = limit;
            drop(s);
            self.changed.notify_waiters();
        }
    }

    /// How many work and how many wait.
    pub(crate) fn counts(&self) -> (usize, usize) {
        let s = self.state.lock();
        (s.working, s.waiting.len())
    }

    /// Wait for a place. `ready(working)` is a last condition checked on its
    /// turn, outside the gate's lock (it may read the system); while it says
    /// no, the turn is kept and it is asked again every couple of seconds.
    pub(crate) async fn enter(self: &Arc<Self>, ready: impl Fn(usize) -> bool) -> Pass {
        let mut ticket = {
            let mut s = self.state.lock();
            let id = s.next;
            s.next += 1;
            s.waiting.push_back(id);
            Ticket {
                gate: self,
                id,
                admitted: false,
            }
        };
        loop {
            // Listen before looking, so a change in between still wakes it.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_turn(&mut ticket, &ready) {
                Turn::In(pass) => return pass,
                Turn::Wait => notified.await,
                Turn::NotReady => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep(RECHECK) => {}
                    }
                }
            }
        }
    }

    fn try_turn(self: &Arc<Self>, ticket: &mut Ticket<'_>, ready: &impl Fn(usize) -> bool) -> Turn {
        let working = {
            let s = self.state.lock();
            if !s.turn_of(ticket.id) {
                return Turn::Wait;
            }
            s.working
        };
        if !ready(working) {
            return Turn::NotReady;
        }
        let mut s = self.state.lock();
        if !s.turn_of(ticket.id) {
            return Turn::Wait;
        }
        s.waiting.pop_front();
        s.working += 1;
        ticket.admitted = true;
        drop(s);
        // The next in line may fit too.
        self.changed.notify_waiters();
        Turn::In(Pass(self.clone()))
    }
}

/// Helpers this machine carries working at once, all chats together: one
/// per 384 MiB of memory and four per processor, 2 to 64 (unknown memory: 4). A helper mostly
/// waits for its model; the heavy ones (a Claude Code process, a build it
/// runs) take a few hundred megabytes and a processor.
pub(crate) fn machine_capacity(ram: Option<u64>, cpus: usize) -> usize {
    let by_ram = ram.map_or(4, |r| (r / (384 << 20)) as usize);
    by_ram.min(cpus.max(1) * 4).clamp(2, 64)
}

/// Memory a new helper must leave free: a quarter of the machine's, at most
/// 4 GiB, leaving room for the server and the user's own work.
fn memory_reserve(ram: Option<u64>) -> u64 {
    ram.map_or(4 << 30, |r| (r / 4).min(4 << 30))
}

/// Whether one more helper may start now: only while the memory left stays
/// above the reserve (unknown: the conservative capacity still applies).
fn memory_room(available: Option<u64>, reserve: u64) -> bool {
    available.is_none_or(|a| a >= reserve)
}

/// A chat's own number (`None`: the default; 0: no limit) as a gate limit.
pub(crate) fn at_once(own: Option<u32>, default: usize) -> Option<usize> {
    match own {
        _ if default == 0 => Some(0),
        None if default >= MAX_LIMIT as usize => None,
        None => Some(default),
        Some(0) => None,
        Some(n) => Some(n as usize),
    }
}

/// A chat's own steps or minutes (`None`: the default; 0: no limit).
pub(crate) fn budget(own: Option<u32>, default: u32) -> u32 {
    match own {
        None if default == 0 => UNLIMITED,
        None => default,
        Some(0) => UNLIMITED,
        Some(n) => n,
    }
}

/// "40 tool calls", or `none` ("no step limit").
pub(crate) fn budget_text(n: u32, unit: &str, none: &str) -> String {
    if n == UNLIMITED {
        none.to_string()
    } else {
        format!("{n} {unit}")
    }
}

fn check_limit(what: &str, n: u32, zero_ok: bool) -> Result<(), ApiError> {
    if n == 0 && !zero_ok {
        let none = if zero_ok { ", or 0 for no limit" } else { "" };
        return Err(bad(format!(
            "{what} must be a whole number from 1 to {MAX_LIMIT}{none}"
        )));
    }
    Ok(())
}

/// The server's gate and each chat's.
pub(super) struct HelperGates {
    pub(super) server: Arc<Gate>,
    chats: Mutex<HashMap<String, ChatGate>>,
    /// Bumped whenever a chat's own limit is written, so a helper that read
    /// the old one never puts it back (see `chat_gate`).
    revision: AtomicU64,
}

struct ChatGate {
    /// The chat's own number, as it was last read (`None`: the default).
    own: Option<u32>,
    gate: Arc<Gate>,
}

impl HelperGates {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            server: Gate::new(Some(capacity)),
            chats: Mutex::new(HashMap::new()),
            revision: AtomicU64::new(0),
        }
    }
}

/// A helper's places while it works: its chat's, its model's lane (models
/// that answer one request at a time) and the server's.
pub(super) struct HelperTurn {
    _chat: Pass,
    _lane: Option<OwnedSemaphorePermit>,
    _server: Pass,
}

/// Helpers working at once on this server, all chats together.
pub(super) fn capacity(cfg: &Config) -> usize {
    match cfg.agent.helper_capacity {
        0 => machine_capacity(
            crate::budget::get().ram,
            std::thread::available_parallelism().map_or(1, |n| n.get()),
        ),
        n => n,
    }
}

fn chat_settings_saved(outcome: crate::store::SaveOutcome) -> Result<(), ApiError> {
    match outcome {
        crate::store::SaveOutcome::Saved => Ok(()),
        crate::store::SaveOutcome::Stale => Err(ApiError::new(
            ErrorCode::Conflict,
            "These helper settings changed elsewhere; reload and try again",
        )),
        crate::store::SaveOutcome::Missing => {
            Err(ApiError::new(ErrorCode::NotFound, "This chat is gone."))
        }
    }
}
impl Inner {
    /// One finite unit, atomically, just before dispatch or Claude's allow
    /// reply. Claude's hook/callback/MCP stages share the exact same call;
    /// native calls always consume their own unit, even with reused ids.
    pub(super) fn reserve_helper_step(
        &self,
        rt: &Rt,
        name: &str,
        args: &serde_json::Value,
        call: Option<&str>,
        dispatch: bool,
    ) -> Result<(), &'static str> {
        let args =
            crate::util::sha256_hex(&serde_json::to_vec(args).expect("JSON value serializes"));
        let mut st = rt.st.lock();
        let Some(b) = st.budget.as_mut() else {
            if st.info.helper_owner == Some(HelperOwner::Tessera) {
                return Err("Not executed: this helper run is not accepting actions.");
            }
            return Ok(());
        };
        if b.closed
            || b.wrapped
            || b.soft_deadline
                .is_some_and(|d| std::time::Instant::now() >= d)
        {
            return Err(STEP_EXHAUSTED);
        }
        if let Some(previous) = call.and_then(|id| b.calls.get_mut(id)) {
            if previous.name != name
                || previous.args != args
                || !previous.pending
                || (dispatch && previous.executed)
            {
                return Err(
                    "Not executed: this action changed, was already dispatched, or finished. Use a new call id.",
                );
            }
            let commit = dispatch && !previous.dispatched;
            previous.dispatched |= dispatch;
            previous.executed |= dispatch;
            if commit {
                st.info.steps = st.info.steps.saturating_add(1);
            }
            drop(st);
            self.save(rt);
            return Ok(());
        }
        if b.max_steps != UNLIMITED {
            if b.used >= b.max_steps {
                return Err(STEP_EXHAUSTED);
            }
            b.used += 1;
        }
        if let Some(id) = call {
            b.calls.insert(
                id.to_string(),
                StepCall {
                    name: name.to_string(),
                    args,
                    dispatched: dispatch,
                    executed: dispatch,
                    pending: true,
                },
            );
            if dispatch {
                st.info.steps = st.info.steps.saturating_add(1);
            }
        }
        drop(st);
        self.save(rt);
        Ok(())
    }

    pub(super) fn rollback_helper_step(&self, rt: &Rt, call: &str) {
        let changed = {
            let mut st = rt.st.lock();
            if let Some(b) = st.budget.as_mut()
                && b.calls.get(call).is_some_and(|c| !c.dispatched)
            {
                b.calls.remove(call);
                if b.max_steps != UNLIMITED {
                    b.used = b.used.saturating_sub(1);
                }
                true
            } else {
                false
            }
        };
        if changed {
            self.save(rt);
        }
    }

    /// One terminal run boundary, after all retries; never on server stop.
    pub(super) fn complete_helper_budget(&self, rt: &Rt) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut st = rt.st.lock();
            let Some(b) = st.budget.as_mut() else {
                return;
            };
            if b.closed {
                return;
            }
            let provisional = b.calls.values().filter(|c| !c.dispatched).count() as u32;
            if b.max_steps != UNLIMITED {
                b.used = b.used.saturating_sub(provisional);
            }
            b.calls.clear();
            b.closed = true;
        }
        self.save(rt);
    }
    pub(super) fn release_provisional_steps(&self, rt: &Rt) {
        let calls: Vec<_> = rt
            .st
            .lock()
            .budget
            .as_ref()
            .map(|b| {
                b.calls
                    .iter()
                    .filter(|(_, c)| !c.dispatched)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for call in calls {
            self.rollback_helper_step(rt, &call);
        }
    }

    /// Validate/commit and queue under the same accounting lock. A failed
    /// send is never a release; another duplicate can still use the claim.
    fn queue_helper_release(
        &self,
        rt: &Rt,
        call: &str,
        queue: impl FnOnce() -> bool,
    ) -> Result<bool, &'static str> {
        let mut st = rt.st.lock();
        let mut new = false;
        if let Some(b) = st.budget.as_mut() {
            if b.closed {
                return Err("Not executed: this helper run is complete.");
            }
            let c = b
                .calls
                .get_mut(call)
                .filter(|c| c.pending)
                .ok_or("Not executed: the action's budget claim is gone.")?;
            new = !c.dispatched;
            c.dispatched = true;
        }
        let queued = queue();
        if new {
            if queued {
                st.info.steps = st.info.steps.saturating_add(1);
            } else {
                st.budget
                    .as_mut()
                    .unwrap()
                    .calls
                    .get_mut(call)
                    .unwrap()
                    .dispatched = false;
            }
        }
        drop(st);
        if queued {
            self.save(rt);
        }
        Ok(queued)
    }

    pub(super) fn finish_helper_step(&self, rt: &Rt, call: &str, tessera_denied: bool) {
        let mut st = rt.st.lock();
        if let Some(b) = st.budget.as_mut()
            && let Some(c) = b.calls.get_mut(call)
            && c.pending
        {
            // Trusted provenance only: Tessera denied this exact call or
            // never queued its release. Independent CLI refusals consume.
            if tessera_denied && c.dispatched && !c.executed {
                if b.max_steps != UNLIMITED {
                    b.used = b.used.saturating_sub(1);
                }
                c.pending = false;
                st.info.steps = st.info.steps.saturating_sub(1);
            } else {
                c.pending = false;
            }
        }
        drop(st);
        self.save(rt);
    }

    fn helper_capacity(&self) -> usize {
        capacity(&self.config.read())
    }

    /// `get_helpers`.
    pub(super) fn helper_settings(&self) -> HelperSettings {
        let a = self.config.read().agent.clone();
        HelperSettings {
            enabled: Some(a.max_subagents != 0),
            at_once: if a.max_subagents >= MAX_LIMIT as usize {
                0
            } else {
                a.max_subagents as u32
            },
            default: crate::budget::get().max_subagents as u32,
            max: MAX_LIMIT,
            steps: Some(a.subagent_steps),
            minutes: Some(a.subagent_minutes),
            capacity: Some(self.helper_capacity() as u32),
        }
    }

    /// `set_helpers`: the defaults of chats that didn't choose, saved in
    /// `config.toml` and used at once.
    pub(super) fn set_helpers(
        &self,
        at_once: u32,
        enabled: Option<bool>,
        steps: Option<u32>,
        minutes: Option<u32>,
    ) -> Result<(), ApiError> {
        check_limit("Helpers working at once", at_once, true)?;
        if let Some(s) = steps {
            check_limit("Steps for each helper", s, true)?;
        }
        if let Some(m) = minutes {
            check_limit("Minutes for each helper", m, true)?;
        }
        {
            let mut c = self.config.write();
            let old = c.agent.clone();
            // Keep legacy config `max_subagents = 0` (helpers off) intact.
            // The wire's 0 (no limit) uses the largest positive count in config.
            c.agent.max_subagents = match (enabled, at_once) {
                (Some(false), _) => 0,
                (Some(true), 0) => MAX_LIMIT as usize,
                // Old clients cannot express unlimited. Their legacy zero
                // must never widen a disabled server, even on a round trip.
                (None, 0) => 0,
                (_, n) => n as usize,
            };
            c.agent.subagent_steps = steps.unwrap_or(old.subagent_steps);
            c.agent.subagent_minutes = minutes.unwrap_or(old.subagent_minutes);
            if let Err(e) = c.save(&self.paths) {
                c.agent = old;
                return Err(internal(e));
            }
        }
        self.refresh_helper_gates();
        tracing::info!("helpers working at once in a chat: {at_once}");
        Ok(())
    }

    /// `set_chat_helpers`: one chat's own limits, used at once (also by its
    /// helpers already waiting).
    pub(super) fn set_chat_helpers(
        &self,
        session_id: &str,
        limits: HelperLimits,
    ) -> Result<SessionInfo, ApiError> {
        for (what, n) in [
            ("Helpers working at once", limits.at_once),
            ("Steps for each helper", limits.steps),
            ("Minutes for each helper", limits.minutes),
        ] {
            if let Some(n) = n {
                check_limit(what, n, true)?;
            }
        }
        let rt = self.rt(session_id)?;
        let (info, state, revision) = {
            let mut st = rt.st.lock();
            if st.info.kind == SessionKind::Subagent {
                return Err(bad(
                    "helpers can't start helpers; set the limits on the chat that started it",
                ));
            }
            st.info.helper_limits = (limits != HelperLimits::default()).then_some(limits);
            st.info.updated_at = now_ms();
            Self::capture_snapshot(&mut st)
        };
        chat_settings_saved(
            self.persist_snapshot(&info, &state, revision)
                .map_err(internal)?,
        )?;
        // After the write, so a helper that read the old limit looks again.
        let gates = &self.helper_gates;
        gates.revision.fetch_add(1, Ordering::SeqCst);
        // Read the latest saved limits, not this writer's possibly superseded
        // value, when concurrent devices change the same chat.
        self.chat_gate(session_id);
        Ok(info)
    }

    /// After the settings changed: the server's capacity and the default
    /// of chats that didn't choose.
    pub(super) fn refresh_helper_gates(&self) {
        let gates = &self.helper_gates;
        gates.revision.fetch_add(1, Ordering::SeqCst);
        gates.server.set_limit(Some(self.helper_capacity()));
        let chats = gates.chats.lock();
        // Read under the gate-map lock: two concurrent server-default writes
        // must not let an older refresh overwrite the newer count.
        let default = self.config.read().agent.max_subagents;
        for c in chats.values() {
            c.gate.set_limit(at_once(c.own, default));
        }
    }

    /// The limits `chat` chose for itself.
    fn chat_limits(&self, chat: &str) -> HelperLimits {
        let info = match self.live_session(chat) {
            Some(rt) => Some(rt.st.lock().info.clone()),
            None => self.store.get_session(chat).ok().flatten().map(|(i, _)| i),
        };
        info.and_then(|i| i.helper_limits).unwrap_or_default()
    }

    /// The chat whose limits a helper counts against: the one that started it.
    fn owning_chat(st: &RtState) -> String {
        st.info
            .parent_id
            .clone()
            .unwrap_or_else(|| st.info.id.clone())
    }

    /// The steps and minutes a helper of `chat` gets ([`UNLIMITED`]: none).
    pub(super) fn helper_budget(&self, chat: &str) -> (u32, u32) {
        let own = self.chat_limits(chat);
        let a = self.config.read().agent.clone();
        (
            budget(own.steps, a.subagent_steps),
            budget(own.minutes, a.subagent_minutes),
        )
    }

    /// `chat`'s gate, with its limit as the chat has it now.
    fn chat_gate(&self, chat: &str) -> Arc<Gate> {
        let gates = &self.helper_gates;
        loop {
            let seen = gates.revision.load(Ordering::SeqCst);
            let own = self.chat_limits(chat).at_once;
            let default = self.config.read().agent.max_subagents;
            let mut chats = gates.chats.lock();
            // A limit written meanwhile: read it again rather than put back
            // the old one.
            if gates.revision.load(Ordering::SeqCst) != seen {
                continue;
            }
            if chats.len() > 32 {
                chats.retain(|_, c| Arc::strong_count(&c.gate) > 1 || c.gate.counts() != (0, 0));
            }
            let c = chats.entry(chat.to_string()).or_insert_with(|| ChatGate {
                own,
                gate: Gate::new(at_once(own, default)),
            });
            c.own = own;
            c.gate.set_limit(at_once(own, default));
            return c.gate.clone();
        }
    }

    /// Show (or stop showing) a helper as waiting for its turn.
    fn show_queued(&self, rt: &Rt, queued: bool) {
        {
            let mut st = rt.st.lock();
            if st.info.helper_queued == queued {
                return;
            }
            st.info.helper_queued = queued;
        }
        self.save(rt);
    }

    /// Wait until this helper may work: its chat's limit, then its model's
    /// lane, then the server's room. Shown as queued only if it has to wait.
    /// Stop, or a permission change before worktree setup, ends the wait.
    pub(super) async fn helper_turn(
        &self,
        rt: &Rt,
        model: &ModelRef,
        cancel: &CancellationToken,
        admission: &ActionAdmission,
        writable_setup: bool,
    ) -> Result<HelperTurn, String> {
        let owner = Self::owning_chat(&rt.st.lock());
        let chat = self.chat_gate(&owner);
        let lane = self.providers.one_at_a_time(model).then(|| {
            self.lanes
                .lock()
                .entry(model.provider.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone()
        });
        let server = self.helper_gates.server.clone();
        let ram = crate::budget::get().ram;
        let reserve = memory_reserve(ram);
        let places = async {
            loop {
                let chat = chat.enter(|_| true).await;
                let lane = match &lane {
                    Some(l) => l.clone().acquire_owned().await.ok(),
                    None => None,
                };
                let server = server
                    .enter(|_| memory_room(crate::budget::mem_available(), reserve))
                    .await;
                // A helper may have passed its chat gate before disable and
                // still be waiting on the lane/server. Recheck at release.
                // Running helpers keep their permits and finish normally.
                if self.config.read().agent.max_subagents != 0 {
                    break HelperTurn {
                        _chat: chat,
                        _lane: lane,
                        _server: server,
                    };
                }
                drop((chat, lane, server));
            }
        };
        let wait = async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err("Stopped before it started.".to_string()),
                _ = admission.changed(), if writable_setup => {
                    Err("The chat's permissions changed before worktree setup.".to_string())
                }
                t = places => Ok(t),
            }
        };
        let result = self.queued_while(rt, wait).await;
        // Admission ended, successfully or not. Worktree setup is work,
        // not a queue; a setup failure must not leave a stale waiting flag.
        self.show_queued(rt, false);
        result
    }

    /// Run `wait`; if it can't finish at once, show the helper as queued
    /// until admission finishes (before any worktree setup).
    async fn queued_while<T>(&self, rt: &Rt, wait: impl Future<Output = T>) -> T {
        tokio::pin!(wait);
        if let std::task::Poll::Ready(v) = futures::poll!(wait.as_mut()) {
            return v;
        }
        self.show_queued(rt, true);
        wait.await
    }

    /// `settings get`, for this chat: its helper limits in words.
    pub(super) fn chat_helper_text(&self, session_id: &str) -> Option<String> {
        let rt = self.live_session(session_id)?;
        let st = rt.st.lock();
        if st.info.kind == SessionKind::Subagent {
            return None;
        }
        let own = st.info.helper_limits.clone().unwrap_or_default();
        drop(st);
        let a = self.config.read().agent.clone();
        let show = |own: Option<u32>, default: u32, unit: &str, none: &str| match own {
            None => format!(
                "{} (server default)",
                budget_text(budget(None, default), unit, none)
            ),
            Some(n) => budget_text(budget(Some(n), default), unit, none),
        };
        Some(format!(
            "# This chat's helpers: {} at once; {} and {} each. `settings set` chat.helpers, chat.helper_steps or chat.helper_minutes (a number, 0 for no limit, null for the server default) only when the user asks.",
            match own.at_once {
                None if a.max_subagents >= MAX_LIMIT as usize =>
                    "no limit (server default)".to_string(),
                None if a.max_subagents == 0 => "disabled (server default)".to_string(),
                None => format!("{} (server default)", a.max_subagents),
                Some(0) => "no limit".to_string(),
                Some(n) => n.to_string(),
            },
            show(own.steps, a.subagent_steps, "tool calls", "no step limit"),
            show(own.minutes, a.subagent_minutes, "minutes", "no time limit"),
        ))
    }

    /// `settings set chat.*` (the user approved): this chat's helper limits.
    pub(super) fn change_chat_setting(
        &self,
        session_id: &str,
        key: &[String],
        value: &Value,
    ) -> Result<String, String> {
        let number = if value.is_null() {
            None
        } else {
            match value.as_u64().or_else(|| match value.as_str() {
                Some(s) if s.trim().eq_ignore_ascii_case("no limit") || s.trim() == "unlimited" => {
                    Some(0)
                }
                _ => None,
            }) {
                Some(n) if n <= MAX_LIMIT as u64 => Some(n as u32),
                _ => {
                    return Err(format!(
                        "`{}` takes a whole number from 1 to {MAX_LIMIT}, 0 for no limit, or null for the server default",
                        key.join(".")
                    ));
                }
            }
        };
        let rt = self.rt(session_id).map_err(|e| e.message)?;
        let mut limits = rt.st.lock().info.helper_limits.clone().unwrap_or_default();
        match key.get(1).map(String::as_str) {
            Some("helpers") if key.len() == 2 => limits.at_once = number,
            Some("helper_steps") if key.len() == 2 => limits.steps = number,
            Some("helper_minutes") if key.len() == 2 => limits.minutes = number,
            _ => {
                return Err(format!(
                    "`{}` is not a chat setting; chat.helpers, chat.helper_steps and chat.helper_minutes are",
                    key.join(".")
                ));
            }
        }
        self.set_chat_helpers(session_id, limits)
            .map_err(|e| e.message)?;
        Ok(match number {
            None => format!("`{}` follows the server default again.", key.join(".")),
            Some(0) => format!(
                "`{}` set to no limit for this chat. Helpers without a limit can use a lot of the user's plan or credits.",
                key.join(".")
            ),
            Some(n) => format!("`{}` set to {n} for this chat.", key.join(".")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn step_reservations_share_exact_cli_calls_but_not_replays_or_changed_inputs() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().budget = Some(super::super::Budget {
            max_steps: 1,
            base: 0,
            used: 0,
            calls: HashMap::new(),
            soft_deadline: None,
            wrapped: false,
            closed: false,
        });
        let args = serde_json::json!({"topic":"bots"});
        let take = |call, dispatch| {
            engine
                .0
                .reserve_helper_step(&rt, "docs", &args, Some(call), dispatch)
        };
        assert!(take("first", false).is_ok()); // PreToolUse
        assert!(take("first", false).is_ok()); // can_use_tool
        assert!(
            engine
                .0
                .reserve_helper_step(
                    &rt,
                    "docs",
                    &serde_json::json!({"topic":"other"}),
                    Some("first"),
                    false
                )
                .is_err()
        );
        engine.0.check_budget(&rt);
        assert!(!rt.st.lock().no_tool_calls); // the admitted call may still dispatch
        assert!(
            engine
                .0
                .queue_helper_release(&rt, "first", || true)
                .unwrap()
        );
        assert_eq!(rt.st.lock().info.steps, 1); // hook release consumes now
        assert!(take("first", true).is_ok());
        assert!(take("first", true).is_err()); // duplicate MCP dispatch
        assert_eq!(take("second", false), Err(STEP_EXHAUSTED));
        assert_eq!(rt.st.lock().budget.as_ref().unwrap().used, 1);
        engine.0.finish_helper_step(&rt, "first", false);
        assert!(take("first", false).is_err()); // completed call id cannot be reused
        engine.0.check_budget(&rt);
        assert!(rt.st.lock().no_tool_calls);
    }

    #[tokio::test]
    async fn no_limit_reserves_no_finite_units() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().budget = Some(super::super::Budget {
            max_steps: UNLIMITED,
            base: 0,
            used: 0,
            calls: HashMap::new(),
            soft_deadline: None,
            wrapped: false,
            closed: false,
        });
        for _ in 0..10 {
            assert!(
                engine
                    .0
                    .reserve_helper_step(&rt, "glob", &serde_json::json!({}), None, true)
                    .is_ok()
            );
        }
        assert_eq!(rt.st.lock().budget.as_ref().unwrap().used, 0);
        engine.0.check_budget(&rt);
        assert!(!rt.st.lock().no_tool_calls);
    }
    async fn settle() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn the_line_moves_in_order_and_follows_a_new_limit() {
        let gate = Gate::new(Some(2));
        let a = gate.enter(|_| true).await;
        let b = gate.enter(|_| true).await;
        // A third waits; a fourth waits behind it.
        let order = Arc::new(Mutex::new(Vec::new()));
        let spawn = |n: u32| {
            let (gate, order) = (gate.clone(), order.clone());
            tokio::spawn(async move {
                let pass = gate.enter(|_| true).await;
                order.lock().push(n);
                pass
            })
        };
        let c = spawn(3);
        settle().await;
        let d = spawn(4);
        settle().await;
        assert_eq!(gate.counts(), (2, 2));
        // Down to one while two work: nobody is stopped, nobody new starts.
        gate.set_limit(Some(1));
        drop(a);
        settle().await;
        assert_eq!(gate.counts(), (1, 2));
        drop(b);
        let c = c.await.unwrap();
        settle().await;
        assert_eq!((gate.counts(), order.lock().clone()), ((1, 1), vec![3]));
        // No limit: the rest start at once.
        gate.set_limit(None);
        let d = d.await.unwrap();
        assert_eq!((gate.counts(), order.lock().clone()), ((2, 0), vec![3, 4]));
        let more: Vec<Pass> =
            futures::future::join_all((0..20).map(|_| gate.enter(|_| true))).await;
        assert_eq!(gate.counts(), (22, 0));
        drop((c, d, more));
        assert_eq!(gate.counts(), (0, 0));
    }

    #[tokio::test]
    async fn a_denied_or_cancelled_provisional_call_returns_its_unit() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().budget = Some(super::super::Budget {
            max_steps: 1,
            base: 0,
            used: 0,
            calls: HashMap::new(),
            soft_deadline: None,
            wrapped: false,
            closed: false,
        });
        let args = serde_json::json!({"topic":"bots"});
        for denial in [
            "snapshot refusal",
            "callback denied",
            "dispatch unavailable",
            "Stop",
        ] {
            let lease = Provisional::new(&engine.0, &rt, denial);
            engine
                .0
                .reserve_helper_step(&rt, "docs", &args, Some(denial), false)
                .unwrap();
            assert_eq!(rt.st.lock().info.steps, 0);
            drop(lease);
            assert_eq!(rt.st.lock().budget.as_ref().unwrap().used, 0);
        }
        engine
            .0
            .reserve_helper_step(&rt, "docs", &args, Some("exec"), true)
            .unwrap();
        engine.0.rollback_helper_step(&rt, "exec");
        assert_eq!(rt.st.lock().info.steps, 1);
        assert_eq!(rt.st.lock().budget.as_ref().unwrap().used, 1);
    }

    #[tokio::test]
    async fn restart_between_hook_release_and_result_keeps_consumption() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().budget = Some(super::super::Budget {
            max_steps: 2,
            base: 0,
            used: 0,
            calls: HashMap::new(),
            soft_deadline: None,
            wrapped: false,
            closed: false,
        });
        let args = serde_json::json!({"pattern":"*"});
        for id in ["never-released", "hook-accepted"] {
            engine
                .0
                .reserve_helper_step(&rt, "Glob", &args, Some(id), false)
                .unwrap();
        }
        assert!(
            engine
                .0
                .queue_helper_release(&rt, "hook-accepted", || true)
                .unwrap()
        );
        let saved = rt.st.lock().budget.as_ref().unwrap().persisted();
        let restored = super::super::Budget::restored(saved);
        assert_eq!(restored.used, 1);
        assert!(!restored.calls.contains_key("never-released"));
        assert!(restored.calls["hook-accepted"].dispatched);
        assert!(!restored.calls["hook-accepted"].pending);
        assert!(!restored.calls["hook-accepted"].executed);
    }
    #[tokio::test]
    async fn restart_keeps_partial_and_exhausted_allowance_and_deadline() {
        for used in [1, 2] {
            let (engine, dir, rt) = super::super::test_support::engine().await;
            let id = rt.st.lock().info.id.clone();
            rt.st.lock().budget = Some(super::super::Budget {
                max_steps: 2,
                base: 0,
                used,
                calls: HashMap::new(),
                soft_deadline: Some(std::time::Instant::now() + Duration::from_secs(60)),
                wrapped: used == 2,
                closed: false,
            });
            engine.0.save(&rt);
            engine.shutdown().await;
            let reopened = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
                .await
                .unwrap();
            let reloaded = reopened.0.rt(&id).unwrap();
            let st = reloaded.st.lock();
            let b = st.budget.as_ref().unwrap();
            assert_eq!((b.max_steps, b.used, b.wrapped), (2, used, used == 2));
            assert!(
                b.soft_deadline
                    .unwrap()
                    .saturating_duration_since(std::time::Instant::now())
                    <= Duration::from_secs(60)
            );
        }
    }
    #[tokio::test]
    async fn leaving_the_line_gives_the_place_to_the_next() {
        let gate = Gate::new(Some(1));
        let working = gate.enter(|_| true).await;
        let first = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter(|_| true).await }
        });
        settle().await;
        let second = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter(|_| true).await }
        });
        settle().await;
        assert_eq!(gate.counts(), (1, 2));
        // The first in line is stopped: its place goes, nothing leaks.
        first.abort();
        assert!(matches!(first.await, Err(e) if e.is_cancelled()));
        assert_eq!(gate.counts(), (1, 1));
        drop(working);
        let _second = second.await.unwrap();
        assert_eq!(gate.counts(), (1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn low_memory_holds_helpers_until_memory_returns() {
        let gate = Gate::new(None);
        let low = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ready = {
            let low = low.clone();
            move |_: usize| {
                let free = if low.load(Ordering::SeqCst) {
                    100
                } else {
                    4000
                };
                memory_room(Some(free), 1000)
            }
        };
        // Even the first waits rather than risking the server's memory.
        let first = tokio::spawn({
            let gate = gate.clone();
            let ready = ready.clone();
            async move { gate.enter(ready).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(gate.counts(), (0, 1));
        low.store(false, Ordering::SeqCst);
        let first = tokio::time::timeout(RECHECK * 3, first)
            .await
            .unwrap()
            .unwrap();
        low.store(true, Ordering::SeqCst);
        let second = tokio::spawn({
            let gate = gate.clone();
            async move { gate.enter(ready).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(gate.counts(), (1, 1));
        // Memory comes back: it starts at its next look.
        low.store(false, Ordering::SeqCst);
        let _second = tokio::time::timeout(RECHECK * 3, second)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(gate.counts(), (2, 0));
        drop(first);
    }

    #[test]
    fn capacity_follows_the_machine_and_numbers_mean_what_they_say() {
        let gib = 1u64 << 30;
        // A 1 GB server, a Raspberry Pi, a 15 GiB/10-core server, a big PC.
        assert_eq!(machine_capacity(Some(gib), 1), 2);
        assert_eq!(machine_capacity(Some(2 * gib), 4), 5);
        assert_eq!(machine_capacity(Some(15 * gib), 10), 40);
        assert_eq!(machine_capacity(Some(64 * gib), 32), 64);
        assert_eq!(machine_capacity(None, 2), 4);
        assert_eq!(memory_reserve(Some(gib)), gib / 4);
        assert_eq!(memory_reserve(Some(16 * gib)), 4 * gib);
        assert!(!memory_room(Some(0), gib) && !memory_room(Some(10), gib));
        assert!(memory_room(None, gib) && memory_room(Some(2 * gib), gib));
        assert_eq!(at_once(None, 4), Some(4));
        assert_eq!(at_once(None, 0), Some(0));
        assert_eq!(at_once(Some(0), 4), None);
        assert_eq!(at_once(Some(12), 4), Some(12));
        assert_eq!(budget(None, 40), 40);
        assert_eq!(budget(Some(0), 40), UNLIMITED);
        assert_eq!(budget(Some(400), 40), 400);
        assert_eq!(
            budget_text(UNLIMITED, "tool calls", "no step limit"),
            "no step limit"
        );
        assert_eq!(
            budget_text(40, "tool calls", "no step limit"),
            "40 tool calls"
        );
        assert!(check_limit("x", 0, false).is_err() && check_limit("x", 0, true).is_ok());
        assert!(check_limit("x", MAX_LIMIT, true).is_ok());
        assert_eq!(at_once(None, MAX_LIMIT as usize), None);
        assert_eq!(budget(None, 0), UNLIMITED);
    }
    #[tokio::test]
    async fn a_missing_question_claim_cannot_commit_or_allow() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().budget = Some(super::super::Budget {
            max_steps: 1,
            base: 0,
            used: 0,
            calls: HashMap::new(),
            soft_deadline: None,
            wrapped: false,
            closed: false,
        });
        let mut owner = Provisional::new(&engine.0, &rt, "question");
        let args = serde_json::json!({"questions":[]});
        engine
            .0
            .reserve_helper_step(&rt, "AskUserQuestion", &args, Some("question"), false)
            .unwrap();
        engine.0.rollback_helper_step(&rt, "question");
        assert!(
            owner
                .release_to(|| panic!("a gone claim cannot queue allow"))
                .is_err()
        );
        assert_eq!(rt.st.lock().info.steps, 0);
    }
    #[tokio::test]
    async fn stale_whole_snapshot_publishes_nothing_and_returns_ui_conflict() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let older = {
            let mut st = rt.st.lock();
            st.info.helper_limits = Some(HelperLimits {
                at_once: Some(1),
                ..Default::default()
            });
            st.info.steps = 1;
            Inner::capture_snapshot(&mut st)
        };
        let newer = {
            let mut st = rt.st.lock();
            st.info.helper_limits = Some(HelperLimits {
                at_once: Some(7),
                ..Default::default()
            });
            st.info.steps = 2;
            Inner::capture_snapshot(&mut st)
        };
        let mut events = engine.subscribe();
        assert_eq!(
            engine
                .0
                .persist_snapshot(&newer.0, &newer.1, newer.2)
                .unwrap(),
            crate::store::SaveOutcome::Saved
        );
        assert!(matches!(
            events.try_recv(),
            Ok(Event::SessionUpdated { .. })
        ));
        let stale = engine
            .0
            .persist_snapshot(&older.0, &older.1, older.2)
            .unwrap();
        assert_eq!(stale, crate::store::SaveOutcome::Stale);
        assert!(
            events.try_recv().is_err(),
            "a stale snapshot publishes no event"
        );
        let err = chat_settings_saved(stale).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert_eq!(
            err.message,
            "These helper settings changed elsewhere; reload and try again"
        );
        let (saved, _) = engine.store().get_session(&newer.0.id).unwrap().unwrap();
        assert_eq!(saved.helper_limits, newer.0.helper_limits);
        assert_eq!(saved.steps, 2);
    }
    #[tokio::test]
    async fn terminal_completion_clears_call_records_but_retry_and_restart_do_not() {
        for limit in [64, UNLIMITED] {
            let (engine, _dir, rt) = super::super::test_support::engine().await;
            rt.st.lock().budget = Some(super::super::Budget {
                max_steps: limit,
                base: 0,
                used: 0,
                calls: HashMap::new(),
                soft_deadline: None,
                wrapped: false,
                closed: false,
            });
            let args = serde_json::json!({"pattern":"*"});
            for i in 0..32 {
                let id = format!("completed-{i}");
                engine
                    .0
                    .reserve_helper_step(&rt, "Glob", &args, Some(&id), true)
                    .unwrap();
                engine.0.finish_helper_step(&rt, &id, false);
            }
            engine
                .0
                .reserve_helper_step(&rt, "Glob", &args, Some("never-released"), false)
                .unwrap();
            // An intermediate report/retry turn returns only provisional
            // claims; completed identities still prevent replay.
            engine.0.release_provisional_steps(&rt);
            let before = rt.st.lock().budget.as_ref().unwrap().persisted();
            assert_eq!(before.calls.len(), 32);
            assert!(!before.closed);
            let restored = super::super::Budget::restored(before);
            assert_eq!(restored.calls.len(), 32);
            assert!(!restored.closed);
            engine.0.stopping.store(true, Ordering::SeqCst);
            engine.0.complete_helper_budget(&rt);
            assert_eq!(
                rt.st.lock().budget.as_ref().unwrap().calls.len(),
                32,
                "planned restart is not terminal completion"
            );
            engine.0.stopping.store(false, Ordering::SeqCst);
            engine.0.complete_helper_budget(&rt);
            assert!(
                engine
                    .0
                    .reserve_helper_step(&rt, "Glob", &args, Some("completed-0"), false)
                    .is_err()
            );
            assert!(
                engine
                    .0
                    .reserve_helper_step(&rt, "Glob", &args, Some("late-new-call"), true)
                    .is_err()
            );
            assert!(
                engine
                    .0
                    .queue_helper_release(&rt, "completed-0", || true)
                    .is_err()
            );
            assert!(
                engine
                    .0
                    .queue_helper_release(&rt, "completed-0", || panic!(
                        "late allow must not queue"
                    ))
                    .is_err()
            );
            let (info, internal) = engine
                .store()
                .get_session(&rt.st.lock().info.id)
                .unwrap()
                .unwrap();
            assert_eq!(info.steps, 32);
            let saved = internal.helper_budget.unwrap();
            assert!(saved.calls.is_empty());
            assert!(saved.closed);
            assert_eq!(saved.used, if limit == UNLIMITED { 0 } else { 32 });
            assert!(super::super::Budget::restored(saved).closed);
        }
    }
    #[tokio::test]
    async fn h0_h1_h0_inflight_snapshots_restore_the_whole_internal_state() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        rt.st.lock().internal.system_prompt = Some("H0".into());
        engine.0.save(&rt);
        let h1 = {
            let mut st = rt.st.lock();
            st.internal.system_prompt = Some("H1".into());
            st.info.steps = 1;
            Inner::capture_snapshot(&mut st)
        };
        let h0 = {
            let mut st = rt.st.lock();
            st.internal.system_prompt = Some("H0".into());
            st.info.steps = 2;
            Inner::capture_snapshot(&mut st)
        };
        // Both snapshots were captured before H1 was persisted. H0 still
        // carries its complete internal value, never an "unchanged" NULL.
        assert_eq!(
            engine.0.persist_snapshot(&h1.0, &h1.1, h1.2).unwrap(),
            crate::store::SaveOutcome::Saved
        );
        assert_eq!(
            engine.0.persist_snapshot(&h0.0, &h0.1, h0.2).unwrap(),
            crate::store::SaveOutcome::Saved
        );
        let (info, internal) = engine.store().get_session(&h0.0.id).unwrap().unwrap();
        assert_eq!(internal.system_prompt.as_deref(), Some("H0"));
        assert_eq!(internal.revision, h0.2);
        assert_eq!(info.steps, 2);
    }

    #[tokio::test]
    async fn host_helper_limits_use_this_chat_and_preserve_unlimited() {
        use crate::tools::ToolHost as _;
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        {
            let mut config = engine.0.config.write();
            config.agent.subagent_steps = 2;
            config.agent.subagent_minutes = 6;
        }
        let chat = rt.st.lock().info.id.clone();
        rt.st.lock().info.helper_limits = Some(HelperLimits {
            steps: Some(0),
            minutes: Some(3),
            ..Default::default()
        });
        let host = super::super::Host(engine.0.clone(), false);
        assert_eq!(host.helper_limits(&chat), (UNLIMITED, 3));
        assert_eq!(host.helper_limits("other-chat"), (2, 6));
        rt.st.lock().info.helper_limits = Some(HelperLimits {
            steps: Some(1),
            minutes: Some(0),
            ..Default::default()
        });
        assert_eq!(host.helper_limits(&chat), (1, UNLIMITED));
    }
    #[tokio::test]
    async fn passed_live_revision_suppresses_a_delayed_publication_after_persistence() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let old = {
            let mut st = rt.st.lock();
            st.info.helper_limits = Some(HelperLimits {
                at_once: Some(1),
                ..Default::default()
            });
            Inner::capture_snapshot(&mut st)
        };
        let persisted = Arc::new(tokio::sync::Barrier::new(2));
        let publish = Arc::new(tokio::sync::Barrier::new(2));
        let (worker, p1, p2) = (engine.clone(), persisted.clone(), publish.clone());
        let delayed = tokio::spawn(async move {
            // Stop exactly after SQLite accepted the old snapshot and before
            // the production publisher acquires the live-state lock.
            assert_eq!(
                worker
                    .0
                    .store
                    .update_session(&old.0, &old.1, old.2)
                    .unwrap(),
                crate::store::SaveOutcome::Saved
            );
            p1.wait().await;
            p2.wait().await;
            worker.0.publish_snapshot(&old.0);
        });
        let mut events = engine.subscribe();
        persisted.wait().await;
        let new = {
            let mut st = rt.st.lock();
            st.info.helper_limits = Some(HelperLimits {
                at_once: Some(7),
                ..Default::default()
            });
            Inner::capture_snapshot(&mut st)
        };
        assert_eq!(
            engine.0.persist_snapshot(&new.0, &new.1, new.2).unwrap(),
            crate::store::SaveOutcome::Saved
        );
        publish.wait().await;
        delayed.await.unwrap();
        let updates: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                Event::SessionUpdated { session } => Some(session),
                _ => None,
            })
            .collect();
        assert_eq!(
            updates.len(),
            1,
            "only the new snapshot publishes: {updates:?}"
        );
        assert_eq!(updates[0].revision, new.2);
        assert_eq!(updates[0].helper_limits, new.0.helper_limits);
        let (info, state) = engine.store().get_session(&new.0.id).unwrap().unwrap();
        assert_eq!(info.revision, new.2);
        assert_eq!(state.revision, new.2);
    }
}
