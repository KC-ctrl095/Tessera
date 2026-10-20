//! Context compaction: the stages are described in `context.rs`.
//!
//! Only stage 2, the summariser, is slow (minutes for a big model and a long
//! history), so it runs ahead of time: when a request crosses the trigger and
//! the cheap stages can't free enough (or before, when that is already
//! certain), the checkpoint is written in the background while the agent
//! keeps working, and it is swapped in at the next step once ready. The
//! agent waits only if the context reaches its hard limit first. Stopping a
//! turn mid-compaction commits nothing: a half-written checkpoint is never
//! saved.

use std::time::Instant;

use super::*;

/// Start the checkpoint early, at this share of the trigger, when the cheap
/// stages can't possibly free enough by the time the trigger is reached.
const EARLY: f64 = 0.85;

/// A checkpoint being written ahead of time (or written and not yet used).
pub(super) struct PreCheckpoint {
    /// The compaction it continues (that entry's seq; 0 when there is none).
    base: u64,
    /// It summarises the history before this seq; the verbatim tail starts here.
    upto: u64,
    cancel: CancellationToken,
    /// `None` while it is being written; then the checkpoint, or `None` when
    /// the summariser failed (the blocking path then tries again).
    rx: tokio::sync::watch::Receiver<Option<Option<String>>>,
}

impl Drop for PreCheckpoint {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// How one region is summarised.
#[derive(Clone)]
struct SummaryJob {
    sid: String,
    model: ModelRef,
    fast: bool,
    /// Summarise entries with `from <= seq < cut`.
    from: u64,
    cut: u64,
    /// The checkpoint so far (the region before `from`).
    previous: Option<String>,
    ledger: Option<TaskLedger>,
    focus: Option<String>,
    /// Size to aim for, in tokens.
    target: u64,
    /// Transcript tokens per summariser call.
    chunk: u64,
}

/// The user stopped the turn while it was summarising.
struct Cancelled;

/// Shows "Compacting" while the agent really waits for the summariser.
struct ShowCompacting<'a> {
    inner: &'a Inner,
    rt: &'a Rt,
    previous: SessionStatus,
}

impl<'a> ShowCompacting<'a> {
    fn new(inner: &'a Inner, rt: &'a Rt) -> Self {
        let previous = rt.st.lock().info.status;
        inner.set_status(rt, SessionStatus::Compacting);
        Self {
            inner,
            rt,
            previous,
        }
    }
}

impl Drop for ShowCompacting<'_> {
    fn drop(&mut self) {
        self.inner.set_status(
            self.rt,
            if self.previous == SessionStatus::Compacting {
                SessionStatus::Idle
            } else {
                self.previous
            },
        );
    }
}

/// The history was edited (the start of the prompt changes anyway): the
/// ledger shown beside the checkpoint catches up at no extra cache cost.
pub(super) fn history_edited(st: &mut RtState) {
    st.internal.ledger_shown = Some(st.info.task.clone().unwrap_or_default());
}

impl Inner {
    pub(super) async fn compact(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        force: bool,
        focus: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let id = rt.st.lock().info.id.clone();
        let entries = self.store.context_history(&id)?;
        let (system, tools, internal, ledger, model, view) = {
            let mut st = rt.st.lock();
            let system = self.system_prompt(&mut st);
            let view = self.view_for(&entries, &st);
            (
                system,
                self.tool_specs(&st),
                st.internal.clone(),
                st.info.task.clone(),
                self.effective_model(&st),
                view,
            )
        };
        let info = self
            .providers
            .model_info(&model)
            .ok_or_else(|| anyhow!("unknown model {model}"))?;
        let cfg = self.context_cfg(&model, info.context_window);
        let budget = context::budget(info.context_window, info.max_output, &cfg);
        let cal = internal.calibration.unwrap_or(1.0);
        let estimate =
            (context::estimate_request(&system, &tools, &view.messages) as f64 * cal) as u64;
        if !force && estimate <= budget.trigger {
            return Ok(());
        }
        // The system prompt and tools: in every request, whatever is compacted.
        let fixed = (context::estimate_request(&system, &tools, &[]) as f64 * cal) as u64;
        self.compact_inner(
            rt,
            &entries,
            &internal,
            ledger.as_ref(),
            estimate,
            fixed,
            budget,
            force,
            focus,
            cancel,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn compact_inner(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        entries: &[Entry],
        internal: &SessionInternal,
        ledger: Option<&TaskLedger>,
        estimate: u64,
        fixed: u64,
        budget: context::Budget,
        force: bool,
        focus: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let cal = internal.calibration.unwrap_or(1.0);
        // Stage 0: stop replaying earlier reasoning. It is usually the bulk of
        // a long tool loop, and dropping it loses no facts (calls and results
        // stay), so try it before summarising anything.
        let thinking = (context::replayed_thinking(entries, internal) as f64 * cal) as u64;
        if !force && thinking > 0 && estimate.saturating_sub(thinking) < budget.trigger * 3 / 4 {
            let next = entries.last().map(|e| e.seq + 1).unwrap_or(0);
            {
                let mut st = rt.st.lock();
                st.internal.thinking_before = next;
                history_edited(&mut st);
            }
            self.save(rt);
            self.notice(
                rt,
                NoticeLevel::Info,
                format!(
                    "Context trimmed: earlier reasoning is no longer replayed (~{}k tokens freed).",
                    thinking / 1000
                ),
            );
            return Ok(());
        }
        // Stage 1: stub old tool outputs.
        let (point, saved) = context::prune_point(entries, internal, budget.keep_recent);
        let saved = (saved as f64 * cal) as u64;
        if !force
            && point > internal.pruned_until
            && estimate.saturating_sub(saved) < budget.trigger * 3 / 4
        {
            {
                let mut st = rt.st.lock();
                st.internal.pruned_until = point;
                // Reasoning written before this edit no longer matches the
                // history it came from (Claude refuses it): stop replaying it.
                st.internal.thinking_before = entries.last().map(|e| e.seq + 1).unwrap_or(0);
                history_edited(&mut st);
            }
            self.save(rt);
            self.notice(rt, NoticeLevel::Info, format!("Context trimmed: older tool output was replaced with references (~{}k tokens freed). The originals remain retrievable.", saved / 1000));
            return Ok(());
        }

        // Stage 2: fold older turns into a checkpoint.
        let previous = context::last_compaction(entries);
        let base = previous.map(|(seq, _, _)| seq).unwrap_or(0);
        let mut from = previous.map(|(_, _, f)| f).unwrap_or(0);
        let keep = if force {
            budget.keep_recent / 2
        } else {
            budget.keep_recent
        };
        let Some(cut) = context::choose_cut(entries, from, keep) else {
            if point > internal.pruned_until {
                {
                    let mut st = rt.st.lock();
                    st.internal.pruned_until = point;
                    st.internal.thinking_before = entries.last().map(|e| e.seq + 1).unwrap_or(0);
                    history_edited(&mut st);
                }
                self.save(rt);
            }
            return Ok(());
        };
        let mut job = self.summary_job(rt, budget, focus, ledger)?;
        job.previous = previous.map(|(_, s, _)| core_of(s).to_string());

        // A checkpoint written ahead of time: use it, wait for it at the hard
        // limit, or start one and keep working until then.
        let hard = hard_limit(&budget);
        if !force {
            let pending = {
                let st = rt.st.lock();
                st.pre
                    .as_ref()
                    .filter(|p| p.base == base && p.upto > from)
                    .map(|p| (p.upto, p.rx.clone()))
            };
            match pending {
                Some((upto, mut rx)) => {
                    let ready = rx.borrow().clone();
                    let written = match ready {
                        Some(r) => r,
                        None if estimate < hard => return Ok(()),
                        None => {
                            let _shown = ShowCompacting::new(self, rt);
                            let started = Instant::now();
                            let r = tokio::select! {
                                r = rx.wait_for(Option::is_some) => r.ok().and_then(|v| v.clone()).flatten(),
                                _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
                            };
                            tracing::info!(
                                "session {}: waited {:.0?} for the checkpoint being written",
                                job.sid,
                                started.elapsed()
                            );
                            r
                        }
                    };
                    rt.st.lock().pre = None;
                    if let Some(core) = written {
                        if upto >= cut {
                            return self.commit(
                                rt, entries, internal, ledger, core, upto, estimate, budget,
                            );
                        }
                        let full = format!("{core}{}", context::appendix(entries, 0, upto));
                        let after = after_tokens(entries, internal, ledger, &full, upto, cal);
                        if fixed + after <= budget.trigger * 3 / 5 {
                            return self.commit(
                                rt, entries, internal, ledger, core, upto, estimate, budget,
                            );
                        }
                        // Too much happened since: summarise only the rest.
                        job.previous = Some(core);
                        from = upto;
                    }
                }
                // Not ahead on a server that takes one request at a time:
                // the agent's steps would queue behind it anyway, each one
                // pushing the other's cached prompt out.
                None if estimate < hard
                    && ahead_pays(&budget, fixed, job.target)
                    && !self.shares_server(rt, &job.model) =>
                {
                    self.start_review(rt);
                    job.from = from;
                    job.cut = cut;
                    self.start_precheckpoint(rt, entries, base, job);
                    return Ok(());
                }
                None => {}
            }
        }

        // Nothing of the region summarised yet (no part of a checkpoint
        // written ahead is reused): the chat's own request covers it as is.
        let whole = from == previous.map(|(_, _, f)| f).unwrap_or(0);
        if whole {
            // Before anything is summarised away, let the memory review keep
            // what is worth keeping (a checkpoint written ahead started it).
            self.start_review(rt);
        }
        let _shown = ShowCompacting::new(self, rt);
        job.from = from;
        job.cut = cut;
        let started = Instant::now();
        let in_place = whole.then_some(&**rt);
        let (core, failed) = match self.summarize(&job, entries, cancel, in_place).await {
            Ok(r) => r,
            Err(Cancelled) => return Err(anyhow!("cancelled")),
        };
        tracing::info!(
            "session {}: checkpoint of entries {}..{} written in {:.0?}",
            job.sid,
            job.from,
            job.cut,
            started.elapsed()
        );
        if let Some(why) = failed {
            self.notice(
                rt,
                NoticeLevel::Warning,
                format!("The summariser failed ({why}); using a structured fallback checkpoint."),
            );
        }
        if cancel.is_cancelled() {
            return Err(anyhow!("cancelled"));
        }
        self.commit(rt, entries, internal, ledger, core, cut, estimate, budget)
    }

    /// Below the compaction trigger, past the observation budget (see
    /// `observation_budget`): old tool output becomes references in one
    /// batch (the newest third of the budget of tool output and the last
    /// replies stay, see `context::observation_point`), and the reasoning
    /// before them is no longer replayed. Only when that frees at least a
    /// quarter of the budget, so the prompt cache misses once per batch;
    /// when it would free less, it looks again after the context grew by
    /// an eighth (`floor`). True when it trimmed.
    pub(super) fn trim_observations(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        prepared: &Prepared,
        floor: &mut u64,
    ) -> bool {
        let (id, model, cal, obs) = {
            let st = rt.st.lock();
            let model = self.effective_model(&st);
            let obs = self.observation_budget(&st, &model);
            (
                st.info.id.clone(),
                model,
                st.internal.calibration.unwrap_or(1.0),
                obs,
            )
        };
        let estimate = (prepared.estimate as f64 * cal) as u64;
        if obs == 0 || estimate <= obs.max(*floor) || self.runs_claude_code(&rt.st.lock()) {
            return false;
        }
        let Some(info) = self.providers.model_info(&model) else {
            return false;
        };
        let cfg = self.context_cfg(&model, info.context_window);
        if obs >= context::budget(info.context_window, info.max_output, &cfg).trigger {
            return false; // compaction comes first
        }
        let Ok(entries) = self.store.context_history(&id) else {
            return false;
        };
        let internal = rt.st.lock().internal.clone();
        let (point, saved) = context::observation_point(&entries, &internal, obs / 3);
        let saved = (saved as f64 * cal) as u64;
        if point <= internal.pruned_until || saved < obs / 4 {
            *floor = estimate + obs / 8;
            return false;
        }
        // The reasoning of the replies kept stays with them (the work in
        // hand); older reasoning goes with the output it was about. Claude
        // refuses reasoning replayed over an edited history: none for it.
        let keep_reasoning =
            self.providers.kind_of(&model.provider) != Some(ProviderKind::Anthropic);
        {
            let mut st = rt.st.lock();
            st.internal.pruned_until = point;
            st.internal.thinking_before = if keep_reasoning {
                st.internal.thinking_before.max(point)
            } else {
                entries.last().map(|e| e.seq + 1).unwrap_or(0)
            };
            history_edited(&mut st);
        }
        self.save(rt);
        self.notice(rt, NoticeLevel::Info, format!("Context trimmed: older tool output was replaced with references (~{}k tokens freed). The originals remain retrievable.", saved / 1000));
        true
    }

    /// Before the trigger: when the cheap stages can't free enough by the
    /// time it is reached (even if everything added until then could be
    /// stubbed), start writing the checkpoint now so it is ready in time.
    pub(super) fn precompact_early(self: &Arc<Self>, rt: &Arc<Rt>, prepared: &Prepared) {
        let estimate = prepared.estimate;
        let (id, model, has_pre, cal) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                self.effective_model(&st),
                st.pre.is_some(),
                st.internal.calibration.unwrap_or(1.0),
            )
        };
        if has_pre || self.runs_claude_code(&rt.st.lock()) {
            return;
        }
        let Some(info) = self.providers.model_info(&model) else {
            return;
        };
        let cfg = self.context_cfg(&model, info.context_window);
        let budget = context::budget(info.context_window, info.max_output, &cfg);
        let estimate = (estimate as f64 * cal) as u64;
        if (estimate as f64) < budget.trigger as f64 * EARLY || estimate > budget.trigger {
            return;
        }
        let fixed =
            (context::estimate_request(&prepared.request.system, &prepared.request.tools, &[])
                as f64
                * cal) as u64;
        let Ok(entries) = self.store.context_history(&id) else {
            return;
        };
        let internal = rt.st.lock().internal.clone();
        let thinking = (context::replayed_thinking(&entries, &internal) as f64 * cal) as u64;
        let (_, saved) = context::prune_point(&entries, &internal, budget.keep_recent);
        let saved = (saved as f64 * cal) as u64;
        let growth = budget.trigger - estimate;
        if thinking + saved + growth >= budget.trigger / 4 {
            return; // a trim may still do
        }
        let previous = context::last_compaction(&entries);
        let base = previous.map(|(seq, _, _)| seq).unwrap_or(0);
        let from = previous.map(|(_, _, f)| f).unwrap_or(0);
        let Some(cut) = context::choose_cut(&entries, from, budget.keep_recent) else {
            return;
        };
        let ledger = rt.st.lock().info.task.clone();
        let Ok(mut job) = self.summary_job(rt, budget, None, ledger.as_ref()) else {
            return;
        };
        if !ahead_pays(&budget, fixed, job.target) || self.shares_server(rt, &job.model) {
            return;
        }
        job.previous = previous.map(|(_, s, _)| core_of(s).to_string());
        job.from = from;
        job.cut = cut;
        self.start_review(rt);
        self.start_precheckpoint(rt, &entries, base, job);
    }

    /// Write the checkpoint for `job` in the background.
    fn start_precheckpoint(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        entries: &[Entry],
        base: u64,
        job: SummaryJob,
    ) {
        let (tx, rx) = tokio::sync::watch::channel(None);
        let cancel = CancellationToken::new();
        rt.st.lock().pre = Some(PreCheckpoint {
            base,
            upto: job.cut,
            cancel: cancel.clone(),
            rx,
        });
        tracing::info!(
            "session {}: writing a checkpoint of entries {}..{} in the background",
            job.sid,
            job.from,
            job.cut
        );
        // Only what the summariser reads: the session itself isn't held.
        let region: Vec<Entry> = entries
            .iter()
            .filter(|e| e.seq >= job.from && e.seq < job.cut)
            .cloned()
            .collect();
        let me = self.clone();
        let checkpoint = async move {
            let started = Instant::now();
            let out = match me.summarize(&job, &region, &cancel, None).await {
                Ok((core, None)) => Some(core),
                Ok((_, Some(why))) => {
                    tracing::warn!("session {}: background checkpoint failed: {why}", job.sid);
                    None
                }
                Err(Cancelled) => return,
            };
            tracing::info!(
                "session {}: background checkpoint ready in {:.0?}",
                job.sid,
                started.elapsed()
            );
            let _ = tx.send(Some(out));
        };
        tokio::spawn(crate::activity::monitor(
            &crate::activity::CHECKPOINT,
            checkpoint,
        ));
    }

    fn start_review(self: &Arc<Self>, rt: &Arc<Rt>) {
        let review = {
            let st = rt.st.lock();
            st.info.kind == SessionKind::Chat
                && !st.info.private
                && st.depth == 0
                && st.internal.user_turns > st.internal.reviewed_turns
                && self.config.read().memory.enabled
                && self.config.read().agent.review_interval > 0
        };
        if review {
            tokio::spawn(self.clone().review(rt.clone()));
        }
    }

    /// The summariser and the sizes for this session's checkpoints.
    fn summary_job(
        &self,
        rt: &Rt,
        budget: context::Budget,
        focus: Option<&str>,
        ledger: Option<&TaskLedger>,
    ) -> Result<SummaryJob> {
        // A private chat is only ever sent to the model the user chose for it.
        let (sid, own, private, fast) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                st.info.model.clone(),
                st.info.private,
                st.info.fast,
            )
        };
        let model = if private {
            own
        } else {
            self.config
                .read()
                .utility_model()
                .filter(|m| self.providers.model_info(m).is_some())
                .unwrap_or(own)
        };
        let window = self
            .providers
            .model_info(&model)
            .map(|m| m.context_window)
            .unwrap_or(budget.window);
        Ok(SummaryJob {
            sid,
            model,
            // The agent waits on it, so it gets the chat's speed.
            fast,
            from: 0,
            cut: 0,
            previous: None,
            ledger: ledger.cloned(),
            focus: focus.map(str::to_string),
            target: ((budget.window as f64) * 0.04).clamp(1_500.0, 6_000.0) as u64,
            chunk: (window as f64 * 0.55) as u64,
        })
    }

    /// Summarise `job`'s region in chunks that fit the summariser, each
    /// updating the checkpoint so far, then condense it if it grew past its
    /// budget. Returns the checkpoint and, when a chunk could not be
    /// summarised (it is then listed in a structured fallback), why.
    async fn summarize(
        &self,
        job: &SummaryJob,
        entries: &[Entry],
        cancel: &CancellationToken,
        in_place: Option<&Rt>,
    ) -> Result<(String, Option<String>), Cancelled> {
        let written = match in_place {
            Some(rt) => self.checkpoint_in_place(rt, job, entries, cancel).await,
            None => None,
        };
        let (core, failed) = match written {
            Some(r) => (Some(r?), None),
            None => self.summarize_transcript(job, entries, cancel).await?,
        };
        let mut core = core.unwrap_or_default();
        // Keep checkpoints bounded however long the task runs: updates tend
        // to accrete, so one well over budget gets a condensing pass.
        if crate::util::estimate_tokens(&core) > job.target * 3 / 2 {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let prompt = context::condense_prompt(&core, job.target);
            match self
                .complete_text(
                    &job.model,
                    context::SUMMARY_SYSTEM,
                    &prompt,
                    cancel,
                    (&job.sid, "compaction"),
                    job.fast,
                    ThinkingLevel::Low,
                )
                .await
            {
                Ok(shorter) => {
                    if let Ok(shorter) = context::accept_summary(&shorter)
                        && shorter.len() < core.len()
                    {
                        core = shorter;
                    }
                }
                Err(_) if cancel.is_cancelled() => return Err(Cancelled),
                Err(_) => {}
            }
        }
        Ok((core, failed))
    }

    /// Summarise `job`'s region from a transcript of it, in chunks that fit
    /// the summariser, each updating the checkpoint so far. Returns the
    /// checkpoint and, when a chunk could not be summarised (it is then
    /// listed in a structured fallback), why.
    async fn summarize_transcript(
        &self,
        job: &SummaryJob,
        entries: &[Entry],
        cancel: &CancellationToken,
    ) -> Result<(Option<String>, Option<String>), Cancelled> {
        let region: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.seq >= job.from && e.seq < job.cut)
            .collect();
        let mut boundaries = Vec::new();
        let mut chunk_start = job.from;
        let mut acc = 0u64;
        for e in &region {
            let t = crate::util::estimate_tokens(&context::transcript_entry(e));
            if acc + t > job.chunk
                && acc > 0
                && matches!(e.body, EntryBody::User { .. } | EntryBody::Assistant { .. })
            {
                boundaries.push((chunk_start, e.seq));
                chunk_start = e.seq;
                acc = 0;
            }
            acc += t;
        }
        boundaries.push((chunk_start, job.cut));
        let mut core = job.previous.clone();
        let mut failed = None;
        for (a, b) in boundaries {
            if cancel.is_cancelled() {
                return Err(Cancelled);
            }
            let transcript = context::transcript(entries, a, b);
            let prompt = context::summary_prompt(
                core.as_deref(),
                job.ledger.as_ref(),
                &transcript,
                job.focus.as_deref(),
                job.target,
            );
            let mut why = String::new();
            let mut produced = None;
            for attempt in 0..2 {
                let p = if attempt == 0 {
                    prompt.clone()
                } else {
                    format!(
                        "{prompt}\n\nIMPORTANT: your previous answer was rejected ({why}). Follow the section format exactly."
                    )
                };
                match self
                    .complete_text(
                        &job.model,
                        context::SUMMARY_SYSTEM,
                        &p,
                        cancel,
                        (&job.sid, "compaction"),
                        job.fast,
                        ThinkingLevel::Low,
                    )
                    .await
                {
                    Ok(text) => match context::accept_summary(&text) {
                        Ok(text) => {
                            produced = Some(text);
                            break;
                        }
                        Err(e) => why = e,
                    },
                    Err(_) if cancel.is_cancelled() => return Err(Cancelled),
                    Err(e) => why = e.to_string(),
                }
            }
            core = Some(match produced {
                Some(text) => text,
                None => {
                    failed = Some(why);
                    context::fallback_summary(entries, a, b, core.as_deref())
                }
            });
        }
        Ok((core, failed))
    }

    /// The checkpoint written by the chat's own model as its next reply (see
    /// [`context::checkpoint_request`]): the request its last step sent, cut
    /// where the verbatim tail starts, plus the request for the checkpoint.
    /// A server on this network that holds that prompt in its cache reads
    /// only the new request, not the whole history again. Only where that is
    /// the cheaper way: the chat's own model on such a server, its tools kept
    /// listed (`tool_choice: none`) or none at all, and little reasoning (a
    /// long think over the checkpoint costs more than reading a transcript).
    /// `None`: not used here, or no usable checkpoint came back (the
    /// transcript way writes it then).
    async fn checkpoint_in_place(
        &self,
        rt: &Rt,
        job: &SummaryJob,
        entries: &[Entry],
        cancel: &CancellationToken,
    ) -> Option<Result<String, Cancelled>> {
        let (model, req) = {
            let mut st = rt.st.lock();
            let model = self.effective_model(&st);
            if model != job.model
                || is_room(&st.info)
                || !self.providers.bounds_output(&model.provider)
            {
                return None;
            }
            let info = self.providers.model_info(&model)?;
            let thinking = st.info.thinking;
            let light = matches!(
                thinking,
                ThinkingLevel::Off | ThinkingLevel::Minimal | ThinkingLevel::Low
            );
            if info.reasoning && !light {
                return None;
            }
            let tools = self.tool_specs(&st);
            if !tools.is_empty() && !self.providers.takes_tool_choice_none(&model) {
                return None;
            }
            let system = self.system_prompt(&mut st);
            let upto = entries.partition_point(|e| e.seq < job.cut);
            let mut messages = self.view_for(&entries[..upto], &st).messages;
            // The previous checkpoint already carries this exact ledger.
            // Keep that cached copy, not another full copy in the nudge.
            let unchanged = st.internal.ledger_shown == job.ledger && job.previous.is_some();
            let mut nudge = context::checkpoint_request(
                job.previous.is_some(),
                if unchanged { None } else { job.ledger.as_ref() },
                job.focus.as_deref(),
                job.target,
            );
            if unchanged {
                nudge.push_str(&format!(
                    "\n{}",
                    crate::tools::meta::unchanged_ledger(&crate::tools::meta::ledger_version(
                        job.ledger.as_ref()
                    ))
                ));
            }
            messages.push(Msg::User(vec![ContentPart::text(nudge)]));
            let req = LlmRequest {
                model: model.model.clone(),
                system,
                messages,
                no_tool_calls: !tools.is_empty(),
                tools,
                thinking,
                max_output: (info.max_output > 0).then_some(info.max_output),
                cache_key: st.info.id.clone(),
                fast: st.info.fast,
                daybreak: st.info.daybreak,
            };
            (model, req)
        };
        match self
            .complete_request(&model, &req, cancel, (&job.sid, "compaction"))
            .await
        {
            Ok(text) => match context::accept_summary(&text) {
                Ok(text) => Some(Ok(core_of(&text).trim().to_string())),
                Err(why) => {
                    tracing::info!(
                        "session {}: the checkpoint written in place was unusable ({why}); writing it from a transcript",
                        job.sid
                    );
                    None
                }
            },
            Err(_) if cancel.is_cancelled() => Some(Err(Cancelled)),
            Err(e) => {
                tracing::warn!(
                    "session {}: the checkpoint in place failed ({e}); writing it from a transcript",
                    job.sid
                );
                None
            }
        }
    }

    /// Append the checkpoint: the history before `cut` is folded into it.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &self,
        rt: &Rt,
        entries: &[Entry],
        internal: &SessionInternal,
        ledger: Option<&TaskLedger>,
        core: String,
        cut: u64,
        estimate: u64,
        budget: context::Budget,
    ) -> Result<()> {
        let cal = internal.calibration.unwrap_or(1.0);
        let summary = format!("{}{}", core.trim(), context::appendix(entries, 0, cut));
        let after_view_tokens = after_tokens(entries, internal, ledger, &summary, cut, cal);
        {
            let mut st = rt.st.lock();
            st.internal.pruned_until = st.internal.pruned_until.max(cut);
            st.internal.system_prompt = None; // pick up new memory/skills at the boundary
            st.internal.bot_prompts.clear();
            st.info.context.compactions += 1;
            history_edited(&mut st);
            // Anything written ahead of time continued the previous checkpoint.
            st.pre = None;
        }
        let entry = self.append(
            rt,
            EntryBody::Compaction {
                external: false,
                summary,
                first_kept_seq: cut,
                tokens_before: estimate,
                tokens_after: after_view_tokens,
            },
        )?;
        // If the verbatim tail alone is still too big (e.g. one huge parallel
        // batch of reads), shorten oversized outputs in it.
        if after_view_tokens > budget.trigger * 9 / 10 {
            let over = after_view_tokens.saturating_sub(budget.trigger / 2);
            let big: Vec<usize> = entries
                .iter()
                .filter(|e| e.seq >= cut)
                .filter_map(|e| match &e.body {
                    EntryBody::ToolResult { content, .. } => Some(plain_text(content).len()),
                    _ => None,
                })
                .filter(|l| *l > 4_000)
                .collect();
            if !big.is_empty() {
                // Bytes each large output may keep so the tail shrinks by `over` tokens.
                let total: usize = big.iter().sum();
                let target_total = total.saturating_sub((over as f64 * 3.6 / cal) as usize);
                let per = (target_total / big.len()).clamp(2_000, 16_000);
                {
                    let mut st = rt.st.lock();
                    st.internal.demote_before = entry.seq;
                    st.internal.demote_bytes = per;
                    history_edited(&mut st);
                }
                self.save(rt);
                self.notice(rt, NoticeLevel::Info, format!("Large recent tool outputs were shortened to about {}KB each to fit the context window (full text retrievable).", per / 1024));
            }
        }
        Ok(())
    }
}

/// Past the trigger the agent works on while a checkpoint is written, up to
/// this: 60% of the way to the window's limit.
fn hard_limit(budget: &context::Budget) -> u64 {
    budget.trigger + budget.limit.saturating_sub(budget.trigger) * 3 / 5
}

/// Whether writing the checkpoint ahead pays off for this budget. A
/// checkpoint swapped in later keeps a longer verbatim tail (what was added
/// while it was written); in a small window that tail would leave too
/// little room before the next compaction, so it is written on the spot.
/// `fixed`: the system prompt and tools, in every request.
fn ahead_pays(budget: &context::Budget, fixed: u64, target: u64) -> bool {
    let tail = budget.keep_recent + hard_limit(budget).saturating_sub(budget.trigger);
    fixed + target + tail <= budget.trigger * 3 / 5
}

/// A checkpoint's own text, without the parts added from the history
/// (verbatim user messages, changed files).
fn core_of(summary: &str) -> &str {
    summary
        .split("\n\n## User Messages (verbatim")
        .next()
        .unwrap_or(summary)
}

/// Estimated tokens of the view once `checkpoint` folds the history before `cut`.
fn after_tokens(
    entries: &[Entry],
    internal: &SessionInternal,
    ledger: Option<&TaskLedger>,
    checkpoint: &str,
    cut: u64,
    cal: f64,
) -> u64 {
    let mut tmp = entries.to_vec();
    tmp.push(Entry {
        seq: u64::MAX,
        ts: 0,
        body: EntryBody::Compaction {
            external: false,
            summary: checkpoint.to_string(),
            first_kept_seq: cut,
            tokens_before: 0,
            tokens_after: 0,
        },
    });
    let v = context::build_view(
        &tmp,
        &SessionInternal {
            pruned_until: internal.pruned_until.max(cut),
            ..internal.clone()
        },
        ledger,
    );
    (context::estimate_msgs(&v.messages) as f64 * cal) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_are_written_ahead_only_where_the_window_has_room() {
        let cfg = crate::config::ContextConfig::default();
        // gpt-6.1-sol's window (a long chat): yes.
        let big = context::budget(272_000, 128_000, &cfg);
        assert!(ahead_pays(&big, 10_000, 6_000));
        assert!(hard_limit(&big) > big.trigger && hard_limit(&big) < big.limit);
        // A local model at 128k: yes.
        assert!(ahead_pays(
            &context::budget(131_072, 8_192, &cfg),
            5_300,
            5_000
        ));
        // A 32k cap: the tail kept would leave too little room; written on the spot.
        let capped = crate::config::ContextConfig {
            max_tokens: 32_000,
            ..Default::default()
        };
        assert!(!ahead_pays(
            &context::budget(131_072, 8_192, &capped),
            5_300,
            1_500
        ));
    }
}
