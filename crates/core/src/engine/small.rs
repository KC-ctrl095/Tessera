//! Model profiles and small model mode in the engine (see `profiles.rs`):
//! when a session starts in small model mode, what it is offered, the one
//! switch to its other tools, plan reminders, the one-call tool check, and
//! what each request to a model carries for its profile. Also the lean start
//! (`[agent] lean_tools`), which offers every chat the same switch.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tessera_proto::*;
use tokio_util::sync::CancellationToken;

use super::{ApiResult, Inner, Rt, RtState, bad, calls, internal, is_room};
use crate::config::ModelTuning;
use crate::llm::{LlmRequest, Msg, ToolSpec};
use crate::profiles::{self, LeanStart, Small};

/// How often each skill was loaded (the store's key-value table).
const SKILL_USES_KEY: &str = "skill_uses";

/// Where the tool check's results are kept (the store's key-value table).
const CHECKS_KEY: &str = "model_checks";

/// How long a check that told nothing (cut off, an error, a busy server)
/// waits before it is tried again.
const RETRY_CHECK_MS: u64 = 6 * 3600 * 1000;

/// The tool check's result for one model (`ok: None`: inconclusive).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Checked {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    at: u64,
}

impl Inner {
    fn checks(&self) -> HashMap<String, Checked> {
        self.store
            .kv_get(CHECKS_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// `model` didn't use a tool in its check.
    fn check_failed(&self, checks: &HashMap<String, Checked>, model: &ModelRef) -> bool {
        checks
            .get(&model.to_string())
            .is_some_and(|c| c.ok == Some(false))
    }

    /// Whether chats on `model` start in small model mode now.
    fn small_for(&self, model: &ModelRef, checks: &HashMap<String, Checked>) -> bool {
        let setting = self.config.read().tuning_for(model).small;
        profiles::small_mode(
            setting.unwrap_or_default(),
            self.providers.kind_of(&model.provider),
            self.providers.own_server(&model.provider),
            &self.providers.profile(model),
            self.check_failed(checks, model),
        )
    }

    /// Small model mode for `st`, decided whenever its prompt is built (and
    /// frozen with it): `base` is everything it may use. Never in private
    /// chats, channels, Tessera's own background sessions or Claude Code;
    /// tools the agent turned on stay on.
    pub(super) fn decide_small(&self, st: &RtState, base: &[ToolSpec]) -> Option<Small> {
        if st.info.private || st.info.internal || is_room(&st.info) {
            return None;
        }
        let model = self.effective_model(st);
        if !self.small_for(&model, &self.checks()) {
            return None;
        }
        let has = |n: &str| base.iter().any(|t| t.name == n);
        Some(Small {
            more: st.internal.small.is_some_and(|s| s.more),
            examples: profiles::EXAMPLE_TOOLS.iter().all(|&t| has(t)),
            replace_only: self.providers.profile(&model).edits == profiles::EditForm::Replace,
        })
    }

    /// The small model mode `st` froze, while it applies (not after the chat
    /// moved to Claude Code, which runs its own harness).
    pub(super) fn small_state(&self, st: &RtState) -> Option<Small> {
        let s = st.internal.small?;
        (self.providers.kind_of(&self.effective_model(st).provider)
            != Some(ProviderKind::ClaudeCode))
        .then_some(s)
    }

    /// `specs` as a small-mode session is offered them: the core tools (all
    /// once turned on) in their simple forms. A session given an explicit
    /// list (an agent type, the memory review) keeps all of it.
    pub(super) fn small_tools(&self, st: &RtState, specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        match self.small_state(st) {
            Some(mut s) => {
                s.more |= st.tools_only.is_some();
                profiles::small_specs(specs, &s)
            }
            None => specs,
        }
    }

    /// `specs` as `st` is offered them: [`small_tools`](Self::small_tools),
    /// and `read` without `paths` where `[tuning] read_paths = false`.
    pub(super) fn shaped_tools(&self, st: &RtState, specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        let mut specs = self.small_tools(st, specs);
        let model = self.effective_model(st);
        if self.config.read().tuning_for(&model).read_paths == Some(false)
            && let Some(read) = specs.iter_mut().find(|t| t.name == "read")
        {
            if let Some(p) = read.parameters["properties"].as_object_mut() {
                p.remove("paths");
            }
            read.parameters["required"] = serde_json::json!(["path"]);
            read.description = read
                .description
                .replace("Read text files", "Read a text file");
        }
        specs
    }

    /// A call in small model mode's flat form (`edit {path, old, new}`,
    /// `task {done}`) in the shape the tool takes, normalized again against
    /// its full schema (line numbers copied from a read dropped, types
    /// fixed). Other calls come back as they are. The stored call keeps the
    /// form the model sent, so its history matches the tools it is offered.
    pub(super) fn full_shape(&self, tool: &Arc<dyn crate::tools::Tool>, args: Value) -> Value {
        let name = tool.name();
        if !matches!(name, "edit" | "task") {
            return args;
        }
        let expanded = profiles::expand_flat(name, args.clone());
        if expanded == args {
            return args;
        }
        let spec = ToolSpec {
            name: name.into(),
            description: String::new(),
            parameters: tool.schema(),
        };
        let mut parts = [AssistantPart::ToolCall {
            id: String::new(),
            name: name.into(),
            arguments: expanded,
        }];
        calls::normalize(&mut parts, std::slice::from_ref(&spec));
        let [part] = parts;
        match part {
            AssistantPart::ToolCall { arguments, .. } => arguments,
            _ => args,
        }
    }

    /// Why a small-mode `edit` that arrived with nothing to change can't run,
    /// in the words of the form it was offered. A model server drops
    /// argument names a tool doesn't list: a model that sent the full
    /// forms (`lines`, `replace`) got back "Provide `lines` or `replace`
    /// edits", sent them again, and looped until the loop guard stopped
    /// it (bench/ hard tier, batch 15: 7-15 such errors per 18 runs).
    pub(super) fn empty_small_edit(
        &self,
        rt: &Rt,
        tool: &Arc<dyn crate::tools::Tool>,
        args: &Value,
    ) -> Option<String> {
        if tool.name() != "edit" {
            return None;
        }
        let small = self.small_state(&rt.st.lock())?;
        let given = |k: &str| match args.get(k) {
            None | Some(Value::Null) => false,
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::String(s)) => !s.is_empty(),
            Some(_) => true,
        };
        if ["replace", "lines", "patch"].iter().any(|k| given(k)) {
            return None;
        }
        Some(profiles::empty_edit_hint(args, small.replace_only))
    }

    /// The worked examples a small-mode conversation opens with, if any.
    pub(super) fn small_examples(&self, st: &RtState) -> Vec<Msg> {
        match self.small_state(st) {
            Some(s) if s.examples => {
                let model = self.effective_model(st);
                profiles::examples(self.config.read().tuning_for(&model).read_paths != Some(false))
            }
            _ => Vec::new(),
        }
    }

    /// Turn the other tools on for a small-mode session (or a lean start),
    /// and the tools of MCP servers that wait until first used, from its
    /// next step (see [`ToolHost::more_tools`](crate::tools::ToolHost::more_tools)).
    /// The tool list changes once, so the prompt cache misses once. `None`:
    /// nothing was kept off.
    pub(super) fn enable_more_tools(&self, session_id: &str) -> Option<bool> {
        let rt = self.rt(session_id).ok()?;
        let (applies, turned_on) = {
            let mut st = rt.st.lock();
            let mut applies = false;
            let mut turned_on = false;
            if let Some(l) = st.internal.mcp_later.as_mut()
                && !l.off.is_empty()
            {
                let off = std::mem::take(&mut l.off);
                l.on.extend(off);
                applies = true;
                turned_on = true;
            }
            let more = if self.small_state(&st).is_some() {
                st.internal.small.as_mut().map(|s| &mut s.more)
            } else if self.lean_start_state(&st).is_some() {
                st.internal.lean_start.as_mut().map(|l| &mut l.more)
            } else {
                None
            };
            if let Some(more) = more {
                applies = true;
                turned_on |= !std::mem::replace(more, true);
            }
            (applies, turned_on)
        };
        if turned_on {
            self.save(&rt);
        }
        applies.then_some(turned_on)
    }

    /// MCP servers whose tools wait until first used in `st` (`mcp.rs`),
    /// decided with its prompt and frozen with it: not in channels, Claude
    /// Code chats or sessions given an explicit tool list. Servers turned on
    /// stay on.
    pub(super) fn decide_mcp_later(&self, st: &RtState) -> Option<crate::mcp::Later> {
        if is_room(&st.info) || st.tools_only.is_some() || self.runs_claude_code(st) {
            return None;
        }
        let on = st
            .internal
            .mcp_later
            .as_ref()
            .map(|l| l.on.clone())
            .unwrap_or_default();
        let mcp: Vec<ToolSpec> = self
            .base_tool_specs(st)
            .into_iter()
            .filter(|t| t.name.starts_with("mcp__"))
            .collect();
        if mcp.is_empty() {
            // After a restart the servers may not be connected yet. Keep
            // the saved on-list until their tools arrive too.
            return (!on.is_empty()).then_some(crate::mcp::Later {
                off: Vec::new(),
                on,
            });
        }
        // The window the chat works in (a 1M model works in 262k), not
        // the model's.
        let model = self.effective_model(st);
        let window = self
            .providers
            .model_info(&model)
            .map(|i| i.context_window)
            .filter(|w| *w > 0)
            .unwrap_or(128_000);
        let window = crate::context::working_window(window, &self.context_cfg(&model, window));
        let servers = self.config.read().mcp_servers.clone();
        let off: Vec<String> = crate::mcp::deferred(&servers, &mcp, window)
            .into_iter()
            .filter(|s| !on.contains(s))
            .collect();
        // Servers turned on stay on through every later prompt build.
        (!off.is_empty() || !on.is_empty()).then_some(crate::mcp::Later { off, on })
    }

    /// MCP servers connected after chats had built their prompts (just after
    /// start): those prompts are built again at their next step, so which
    /// servers' tools wait (and the prompt's line about them) takes them in.
    /// Their tool list changes then anyway, so the prompt cache misses no
    /// more than it would. Not for Claude Code chats.
    pub(super) fn mcp_connected(&self) {
        for rt in self.live_sessions() {
            let mut st = rt.st.lock();
            if st.internal.system_prompt.is_some() && !self.runs_claude_code(&st) {
                st.internal.system_prompt = None;
                st.internal.bot_prompts.clear();
            }
        }
    }

    /// `specs` without the tools of MCP servers that wait (`st`'s frozen
    /// decision; not once the chat moved to Claude Code).
    pub(super) fn mcp_later_tools(&self, st: &RtState, mut specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        let Some(l) = st.internal.mcp_later.as_ref().filter(|l| !l.off.is_empty()) else {
            return specs;
        };
        if self.runs_claude_code(st) {
            return specs;
        }
        let servers = self.config.read().mcp_servers.clone();
        specs.retain(|t| {
            !t.name.starts_with("mcp__")
                || crate::mcp::server_of(&t.name, &servers)
                    .is_none_or(|s| !l.off.iter().any(|o| o == s))
        });
        specs
    }

    /// The prompt's line about the MCP servers whose tools wait.
    pub(super) fn mcp_later_prompt_line(&self, st: &RtState) -> Option<String> {
        let l = st.internal.mcp_later.as_ref()?;
        let mcp: Vec<ToolSpec> = self
            .base_tool_specs(st)
            .into_iter()
            .filter(|t| t.name.starts_with("mcp__"))
            .collect();
        crate::mcp::later_line(&l.off, &mcp, &self.config.read().mcp_servers)
    }

    /// A call to a tool of an MCP server that waits until first used: that
    /// server's tools are on from the next step (`None`: not such a call).
    fn mcp_turned_on_by_call(&self, rt: &Rt, name: &str) -> Option<String> {
        if !name.starts_with("mcp__") {
            return None;
        }
        let server = {
            let mut st = rt.st.lock();
            if st.no_tool_calls || !self.base_tool_specs(&st).iter().any(|t| t.name == name) {
                return None;
            }
            let servers = self.config.read().mcp_servers.clone();
            let server = crate::mcp::server_of(name, &servers)?.to_string();
            let l = st.internal.mcp_later.as_mut()?;
            let i = l.off.iter().position(|s| *s == server)?;
            l.off.remove(i);
            l.on.push(server.clone());
            server
        };
        self.save(rt);
        Some(format!(
            "`{name}` was off until first used; the `{server}` MCP tools are on from your next step. Call it again."
        ))
    }

    /// A call to a tool small model mode or a lean start kept off (which
    /// turns them all on, waiting MCP tools too), or to a tool of an MCP
    /// server that waits until first used: says so (`None`: not such a
    /// call).
    pub(super) fn turned_on_by_call(&self, rt: &Rt, name: &str) -> Option<String> {
        self.kept_off_turned_on(rt, name)
            .or_else(|| self.mcp_turned_on_by_call(rt, name))
    }

    /// [`turned_on_by_call`](Self::turned_on_by_call) for small model mode
    /// and a lean start.
    fn kept_off_turned_on(&self, rt: &Rt, name: &str) -> Option<String> {
        let id = {
            let st = rt.st.lock();
            let more = match (self.small_state(&st), self.lean_start_state(&st)) {
                (Some(s), _) => s.more,
                (None, Some(l)) => l.more,
                (None, None) => return None,
            };
            if more || st.no_tool_calls {
                return None;
            }
            let full = self.base_tool_specs(&st);
            full.iter()
                .any(|t| t.name == name)
                .then(|| st.info.id.clone())?
        };
        self.enable_more_tools(&id)?;
        Some(format!(
            "`{name}` was off to keep things simple; it and the other tools are on from your next step. Call it again."
        ))
    }

    /// A lean start for `st` (`[agent] lean_tools`), decided with its
    /// prompt after small model mode and frozen with it: not where small
    /// model mode applies, nor in private chats, channels, Tessera's own
    /// background sessions, terminal chats (their own lean set already),
    /// sessions given an explicit tool list or Claude Code. Tools the agent
    /// turned on stay on.
    pub(super) fn decide_lean_start(&self, st: &RtState) -> Option<LeanStart> {
        if !self.config.read().agent.lean_tools
            || st.internal.small.is_some()
            || st.info.lean
            || st.info.private
            || st.info.internal
            || is_room(&st.info)
            || st.tools_only.is_some()
            || self.providers.kind_of(&self.effective_model(st).provider)
                == Some(ProviderKind::ClaudeCode)
        {
            return None;
        }
        Some(LeanStart {
            more: st.internal.lean_start.is_some_and(|l| l.more),
        })
    }

    /// The lean start `st` froze, while it applies (not after the chat moved
    /// to Claude Code, nor under small model mode).
    pub(super) fn lean_start_state(&self, st: &RtState) -> Option<LeanStart> {
        let l = st.internal.lean_start?;
        (self.small_state(st).is_none()
            && self.providers.kind_of(&self.effective_model(st).provider)
                != Some(ProviderKind::ClaudeCode))
        .then_some(l)
    }

    /// `specs` as a lean start offers them (see `profiles::lean_start_specs`).
    pub(super) fn lean_start_tools(&self, st: &RtState, specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        match self.lean_start_state(st) {
            Some(l) => profiles::lean_start_specs(specs, &l),
            None => specs,
        }
    }

    /// The prompt's line about the tools a lean start keeps off for now.
    pub(super) fn lean_start_prompt_line(&self, st: &RtState) -> Option<String> {
        if self.lean_start_state(st)?.more {
            return None;
        }
        let base = self.base_tool_specs(st);
        let names: Vec<&str> = base.iter().map(|t| t.name.as_str()).collect();
        profiles::lean_start_line(&names)
    }

    /// The `skill` tool loaded `name`: one more use (a capped skills index
    /// lists the most used first).
    pub(super) fn count_skill_use(&self, name: &str) {
        let mut uses = self.skill_uses();
        *uses.entry(name.to_string()).or_default() += 1;
        if let Ok(v) = serde_json::to_string(&uses) {
            let _ = self.store.kv_set(SKILL_USES_KEY, &v);
        }
    }

    pub(super) fn skill_uses(&self) -> HashMap<String, u64> {
        self.store
            .kv_get(SKILL_USES_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    /// The plan reminder added to a small-mode turn's results every few
    /// steps while the task ledger has open steps (appended, never in the
    /// prompt's start, so caches hold).
    pub(super) fn plan_note(&self, rt: &Rt, step: u32) -> Option<String> {
        if step == 0 || !step.is_multiple_of(profiles::PLAN_EVERY) {
            return None;
        }
        let st = rt.st.lock();
        self.small_state(&st)?;
        profiles::plan_line(st.info.task.as_ref()?)
    }

    /// Identical failing calls before the next is refused in this turn.
    pub(super) fn repeat_limit(&self, rt: &Rt) -> Option<u32> {
        self.small_state(&rt.st.lock())
            .map(|_| profiles::SMALL_REPEAT_LIMIT)
    }

    /// What a request of `rt`'s to `model` carries beside the conversation:
    /// the model card's sampling values unless `[tuning] sampling = false`,
    /// the user's own values, and one call per reply in small model mode
    /// (`[tuning] parallel_tool_calls` over it). Several calls per reply
    /// halved qwen3.8-27b's requests on long tasks, but it then got tasks
    /// that follow a written spec wrong far more often (bench/, batch 15:
    /// 6 of 14 runs passed with them, 19 of 20 without): its reads go
    /// together through `read {paths}` instead.
    pub(super) fn request_tuning(&self, rt: &Rt, model: &ModelRef) -> profiles::Tuning {
        let t = self.config.read().tuning_for(model);
        let single = self.small_state(&rt.st.lock()).is_some();
        profiles::Tuning {
            card_sampling: t.sampling.unwrap_or(true),
            sampling: profiles::Sampling::of(&t),
            parallel_calls: t.parallel_tool_calls.or(single.then_some(false)),
        }
    }

    /// Whether a reply's calls after a failed change are held back (and
    /// identical calls in one reply run once): small model mode (where
    /// several calls per reply were turned on), unless
    /// `[tuning] reply_guard = false`.
    pub(super) fn reply_guard(&self, rt: &Rt) -> bool {
        let model = self.effective_model(&rt.st.lock());
        self.small_state(&rt.st.lock()).is_some()
            && self.config.read().tuning_for(&model).reply_guard != Some(false)
    }

    /// `[context]` for a chat on `model` (window `window`), with the length
    /// its profile says it still works well with (`[tuning] context_budget`
    /// over it; 0: none).
    pub(super) fn context_cfg(
        &self,
        model: &ModelRef,
        window: u64,
    ) -> crate::config::ContextConfig {
        let (mut cfg, set) = {
            let c = self.config.read();
            (c.context.clone(), c.tuning_for(model).context_budget)
        };
        cfg.budget = match set {
            Some(n) => n,
            None if self.providers.kind_of(&model.provider) == Some(ProviderKind::ClaudeCode) => 0,
            None => self
                .providers
                .profile(model)
                .context_budget(window)
                .unwrap_or(0),
        };
        cfg
    }

    /// Tokens past which `st`'s old tool output is replaced with references
    /// in one batch, well before compaction (`[tuning] observation_budget`;
    /// by default [`profiles::OBSERVE_AT`] in small model mode; 0: off).
    pub(super) fn observation_budget(&self, st: &RtState, model: &ModelRef) -> u64 {
        match self.config.read().tuning_for(model).observation_budget {
            Some(n) => n,
            None if self.small_state(st).is_some() => profiles::OBSERVE_AT,
            None => 0,
        }
    }

    /// The model lists with each model's small model mode filled in.
    pub(super) fn with_small_modes(&self, mut infos: Vec<ProviderInfo>) -> Vec<ProviderInfo> {
        let checks = self.checks();
        let cfg = self.config.read().clone();
        for p in &mut infos {
            let own = self.providers.own_server(&p.id);
            for m in &mut p.models {
                let r = ModelRef {
                    provider: p.id.clone(),
                    model: m.id.clone(),
                };
                m.small_mode = cfg.tuning_for(&r).small.unwrap_or_default();
                m.small = profiles::small_mode(
                    m.small_mode,
                    Some(p.kind),
                    own,
                    &self.providers.profile(&r),
                    self.check_failed(&checks, &r),
                );
            }
        }
        infos
    }

    /// `SetModelSmall`: saved as `[tuning."provider/model"] small` (`Auto`
    /// removes it). Open chats keep their mode until their next checkpoint.
    pub(super) fn set_model_small(
        &self,
        provider: &str,
        model: &str,
        mode: SmallMode,
    ) -> ApiResult {
        let (provider, model) = (provider.trim(), model.trim());
        if provider.is_empty() || model.is_empty() || provider.contains('/') {
            return Err(bad("name the model as a provider id and a model id"));
        }
        let key = format!("{provider}/{model}");
        {
            let mut c = self.config.write();
            let entry = c.tuning.entry(key.clone()).or_default();
            entry.small = (mode != SmallMode::Auto).then_some(mode);
            if *entry == ModelTuning::default() {
                c.tuning.remove(&key);
            }
            c.save(&self.paths).map_err(internal)?;
        }
        let _ = self.events.send(Event::ProvidersChanged);
        Ok(Value::Null)
    }

    /// The one-call tool check: the first time a model on the user's own
    /// server is used for a chat, it is asked once, with one tool, "What time
    /// is it? Use the tool." (well under 1k tokens). A model that answers in
    /// text instead (or calls something else) is noted, the chat tells the
    /// user in plain words, and the model runs in small model mode from then
    /// on. Only when nothing else of Tessera's is waiting on that server,
    /// only for the model being used, and only once (`[tuning] check = false`
    /// skips it).
    pub(super) async fn check_model_once(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        cancel: &CancellationToken,
    ) {
        let (sid, model, skip) = {
            let st = rt.st.lock();
            // A long chat's prompt, cached on a one-slot server, isn't
            // pushed out for it: the check waits for a new or short chat.
            let skip = st.depth > 0
                || st.info.internal
                || is_room(&st.info)
                || st.info.context.tokens > 8_000;
            (st.info.id.clone(), self.effective_model(&st), skip)
        };
        if skip
            || !self.providers.own_server(&model.provider)
            || self.config.read().tuning_for(&model).check == Some(false)
        {
            return;
        }
        let key = model.to_string();
        let mut checks = self.checks();
        if checks
            .get(&key)
            .is_some_and(|c| c.ok.is_some() || crate::util::now_ms() < c.at + RETRY_CHECK_MS)
        {
            return;
        }
        // Another chat busy on that server: not now (it would wait, and push
        // that chat's cached prompt out).
        let sessions: Vec<Arc<Rt>> = self.sessions.lock().values().cloned().collect();
        let busy = sessions.iter().any(|other| {
            let st = other.st.lock();
            st.info.id != sid && st.running && self.effective_model(&st).provider == model.provider
        });
        if busy {
            return;
        }
        let tools = profiles::check::tools();
        let req = LlmRequest {
            model: model.model.clone(),
            system: profiles::check::SYSTEM.into(),
            messages: vec![Msg::User(vec![ContentPart::text(profiles::check::PROMPT)])],
            tools: tools.clone(),
            thinking: self.quickest_thinking(&model),
            max_output: Some(1024),
            cache_key: "util-check".into(),
            fast: false,
            daybreak: false,
            no_tool_calls: false,
        };
        let tuning = profiles::Tuning {
            parallel_calls: Some(false),
            ..self.request_tuning(rt, &model)
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let asked = self
            .providers
            .stream_tuned(&model, &req, &tuning, &tx, cancel);
        let ok = match tokio::time::timeout(std::time::Duration::from_secs(180), asked).await {
            Ok(Ok(r)) if r.stop != StopReason::Aborted && !cancel.is_cancelled() => {
                let mut logged = r.usage.clone();
                logged.cost_usd = self.price_of(&model).map(|p| p.cost(&logged));
                self.log_usage(Some(&sid), &model, "other", &logged);
                let mut content = r.content;
                calls::recover_text_calls(&mut content, &tools);
                calls::normalize(&mut content, &tools);
                if profiles::check::passed(&content) {
                    Some(true)
                } else if r.stop == StopReason::Length {
                    // Still thinking at the limit: tells nothing.
                    None
                } else {
                    Some(false)
                }
            }
            Ok(Err(e)) => {
                tracing::info!("tool check of {key} could not run: {e}");
                None
            }
            _ => None,
        };
        if cancel.is_cancelled() {
            return;
        }
        tracing::info!("tool check of {key}: {ok:?}");
        checks.insert(
            key,
            Checked {
                ok,
                at: crate::util::now_ms(),
            },
        );
        if let Ok(v) = serde_json::to_string(&checks) {
            let _ = self.store.kv_set(CHECKS_KEY, &v);
        }
        if ok == Some(false) {
            let name = self
                .providers
                .model_info(&model)
                .map_or_else(|| model.model.clone(), |i| i.name);
            self.notice(
                rt,
                NoticeLevel::Warning,
                profiles::check::failed_notice(&name),
            );
            let _ = self.events.send(Event::ProvidersChanged);
        }
    }
}
