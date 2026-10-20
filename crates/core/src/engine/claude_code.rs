//! Chat turns run by the user's own Claude Code (`crate::claude_code`):
//! one `claude -p` process per turn, in the chat's folder and the command
//! sandbox, continuing its Claude Code conversation (`--resume`). Its stream
//! becomes ordinary transcript entries and live events, its permission
//! prompts become Tessera approvals, and its usage is recorded as the plan's.
//!
//! Tessera's own tools come along over the turn's pipe (the bridge): Claude
//! Code's `mcp_message` requests are answered with [`Inner::execute_tool_ext`],
//! the dispatch Tessera's own agent uses, on this chat's session, so its
//! approvals, refusals, credential and machine rules apply unchanged.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::*;
use crate::claude_code::{self as cc, Link};
use crate::config::VerifyCheck;
use crate::llm::ToolSpec;

mod limits;
#[cfg(test)]
mod native_tests;

/// How long a stopped turn may take to wind down before it is killed.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(8);
/// Once what Claude Code left running in the background is done, how long it
/// may stay quiet before its input closes (it takes the turn up again by itself).
const BACKGROUND_GRACE: std::time::Duration = std::time::Duration::from_secs(20);
/// Right after background work it waited for ended, Claude Code takes the
/// turn up again by itself; its first words may take a while then (a long
/// conversation, a prompt cache gone cold while the work ran).
const RESUME_GRACE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Nothing is left in the background and Claude Code has been quiet long
/// enough to call its run over (see [`BACKGROUND_GRACE`], [`RESUME_GRACE`]).
fn gone_quiet(tasks_left: bool, silent: std::time::Duration, carry_on_expected: bool) -> bool {
    !tasks_left
        && silent
            >= if carry_on_expected {
                RESUME_GRACE
            } else {
                BACKGROUND_GRACE
            }
}
/// The longest a turn's run waits for its background work: hours of work
/// (upgrades, builds, subagents) are normal, and closing its input while
/// that work runs breaks what Claude Code does when it ends. The chat shows
/// as idle meanwhile, so even a dev server left running costs nothing.
const BACKGROUND_MAX: std::time::Duration = std::time::Duration::from_secs(12 * 3600);
/// Whether a turn's run still waits for what Claude Code left running in the
/// background, keeping its input open meanwhile. Closing it too early is
/// what breaks it: when the work ends, Claude Code takes the turn up again by
/// itself, and then its approvals and Tessera's tools fail ("Stream closed").
#[derive(Default)]
struct BackgroundWait {
    /// Its turn ended with work still running: since when.
    since: Option<tokio::time::Instant>,
    /// Last count shown while waiting; quiet ticks don't repeat notices.
    shown: Option<(usize, bool)>,
}

#[derive(Debug, PartialEq)]
enum BackgroundTick {
    Expired,
    Stale,
    Current,
}

impl BackgroundWait {
    /// Expiry takes precedence over context admission: stale runs must still end.
    fn tick(&self, stale: bool) -> BackgroundTick {
        if self.expired() {
            BackgroundTick::Expired
        } else if stale {
            BackgroundTick::Stale
        } else {
            BackgroundTick::Current
        }
    }

    fn waiting(&self) -> bool {
        self.since.is_some()
    }

    /// Its turn ended with work still running (the first such end counts).
    fn start(&mut self) {
        self.since.get_or_insert_with(tokio::time::Instant::now);
    }

    fn stop(&mut self) {
        self.since = None;
        self.shown = None;
    }

    fn count_changed(&mut self, count: usize, retained: bool) -> bool {
        let changed = self.shown != Some((count, retained));
        self.shown = Some((count, retained));
        changed
    }

    /// A message from Claude Code's own turn (not a subagent's): it is
    /// working on the turn. (A context line it echoes is no turn.)
    fn takes_turn(v: &Value) -> bool {
        let kind = v.get("type").and_then(Value::as_str);
        Self::answers(v)
            || (kind == Some("user")
                && v.get("parent_tool_use_id").is_none_or(Value::is_null)
                && v.get("isReplay").and_then(Value::as_bool) == Some(true)
                && v.get("isSynthetic").and_then(Value::as_bool) != Some(true))
    }

    /// Its own model at work (not a subagent's): a turn it is in, which ends
    /// with a result.
    fn answers(v: &Value) -> bool {
        let kind = v.get("type").and_then(Value::as_str);
        v.get("parent_tool_use_id").is_none_or(Value::is_null)
            && (matches!(kind, Some("assistant" | "stream_event"))
                || (kind == Some("system")
                    && v.get("subtype").and_then(Value::as_str) == Some("init")))
    }

    /// A line from Claude Code. When it took the turn up again by itself (a
    /// background task ended) it is busy, not waiting, until its next
    /// result: the clock starts over then, however long this wait was.
    /// `true` when this line ended the wait.
    fn heard(&mut self, v: &Value) -> bool {
        let carried_on = self.waiting() && Self::takes_turn(v);
        if carried_on {
            self.stop();
        }
        carried_on
    }

    /// Waited too long (see [`BACKGROUND_MAX`]).
    fn expired(&self) -> bool {
        self.since.is_some_and(|s| s.elapsed() >= BACKGROUND_MAX)
    }
}

/// What a subagent's session says when the user stopped it alone.
const STOPPED_ALONE: &str = "Stopped by the user; the chat's turn went on without it.";
/// Files bigger than this get no copy before Claude Code changes them.
const MAX_SNAPSHOT: u64 = 16 << 20;
/// Claude Code processes kept open between turns at most (see [`ClaudeProcs`]).
const MAX_PARKED: usize = 3;
/// Another `claude` was renewing the shared sign-in: try again after this.
const REFRESH_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

impl Inner {
    /// The session's turns run in Claude Code.
    pub(super) fn runs_claude_code(&self, st: &RtState) -> bool {
        self.providers.kind_of(&self.effective_model(st).provider) == Some(ProviderKind::ClaudeCode)
    }

    /// The Claude account a provider (`claude-code`, or another account of
    /// the user's) runs as: its `claude` command and its own folder.
    pub(super) fn claude_account(&self, provider: &str) -> cc::Account {
        match self
            .providers
            .configs()
            .into_iter()
            .find(|c| c.id == provider)
        {
            Some(c) => self.providers.claude_account(&c),
            None => cc::Account::default(),
        }
    }

    /// `claude_code_status`: installed? signed in? (asked again on
    /// `refresh`), for one account (default: the usual one).
    pub(super) async fn claude_code_status(
        &self,
        refresh: bool,
        provider: Option<&str>,
    ) -> Result<ClaudeCodeInfo, ApiError> {
        let provider = provider.unwrap_or(cc::PROVIDER);
        if provider != cc::PROVIDER
            && self.providers.kind_of(provider) != Some(ProviderKind::ClaudeCode)
        {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!("no Claude account `{provider}`"),
            ));
        }
        let account = self.claude_account(provider);
        let d = match self.providers.claude_code(&account.key()) {
            Some(d) if !refresh => d,
            _ => self.claude_code_detect(&account).await,
        };
        Ok(cc::info(&d, &crate::node::hostname(), provider, &account))
    }

    /// Ask every configured Claude account about itself again (a refreshed
    /// model list).
    pub(super) async fn claude_code_refresh(&self) {
        let accounts: Vec<cc::Account> = self
            .providers
            .configs()
            .iter()
            .filter(|c| c.kind == ProviderKind::ClaudeCode)
            .map(|c| self.providers.claude_account(c))
            .collect();
        for a in accounts {
            self.claude_code_detect(&a).await;
        }
    }

    /// Run `claude` to ask it about an account, and keep the answer.
    pub(super) async fn claude_code_detect(&self, account: &cc::Account) -> cc::Detection {
        let d = cc::detect(account).await;
        self.claude_code_known(&account.key(), d.clone());
        d
    }

    /// Keep what is known about a Claude account (by `Account::key`; also
    /// for after a restart, so nothing has to run `claude` then) and tell
    /// clients when it changed.
    fn claude_code_known(&self, key: &str, d: cc::Detection) {
        if !self.providers.set_claude_code(key, d) {
            return;
        }
        if let Ok(json) = serde_json::to_string(&self.providers.claude_code_all()) {
            let _ = self.store.kv_set("claude_code_detected", &json);
        }
        let _ = self.events.send(Event::ProvidersChanged);
    }

    /// What a turn showed about an account's sign-in (it failed for want of
    /// one, or worked).
    fn claude_code_signed_in(&self, key: &str, signed_in: bool) {
        if let Some(mut d) = self.providers.claude_code(key) {
            d.signed_in = signed_in;
            self.claude_code_known(key, d);
        }
    }

    /// What earlier runs learnt: Claude Code's model list and whether it is
    /// installed and signed in. Nothing runs `claude` here.
    pub(super) fn claude_code_restore(&self) {
        let accounts: Vec<String> = self
            .providers
            .configs()
            .into_iter()
            .filter(|c| c.kind == ProviderKind::ClaudeCode)
            .map(|c| c.id)
            .collect();
        for id in accounts {
            if let Ok(Some(json)) = self.store.kv_get(&claude_models_key(&id))
                && let Ok(mut models) = serde_json::from_str::<Vec<ModelInfo>>(&json)
            {
                // Saved by an older Tessera that thought every model had 200K.
                for m in &mut models {
                    m.context_window = m.context_window.max(cc::context_window(&m.id));
                    m.provider = id.clone();
                }
                self.providers.set_discovered(&id, models);
            }
        }
        if let Ok(Some(json)) = self.store.kv_get("claude_code_detected")
            && let Ok(known) = serde_json::from_str::<HashMap<String, cc::Detection>>(&json)
        {
            for (key, d) in known {
                self.providers.set_claude_code(&key, d);
            }
        }
    }

    /// Tessera's tools offered to Claude Code in this chat: what Tessera's
    /// own agent is offered here (a bot's preset, the permission mode,
    /// `machines` once there is one…), less what Claude Code has itself.
    /// Private chats expose only the chosen search provider, never local tools.
    pub(super) fn claude_bridge_tools(&self, st: &RtState) -> Vec<ToolSpec> {
        let protected = !self.redaction_secrets().is_empty();
        let search = self.claude_tessera_search() || protected;
        self.callable_tools(st)
            .into_iter()
            // Private CLI chats use their built-ins, with only an explicitly
            // selected public search bridged. Preserve that narrower contract
            // even when callable_tools offers a public reader to native chats.
            .filter(|t| {
                (!st.info.private || t.name == "web_search" || (protected && t.name == "web_fetch"))
                    && (cc::bridged(&t.name) || (t.name == "web_search" && search))
            })
            .map(|mut t| {
                t.description = cc::bridge_description(&t.name, &t.description);
                if t.name == "save_secret" {
                    t.description = "List credential names, or ask the user to enter a service key or login securely in Tessera. Never send values here. Never collect Claude sign-in credentials.".into();
                    t.parameters = crate::tools::secrets::entry_schema();
                }
                t
            })
            .collect()
    }

    /// An explicit provider choice, or an installed provider in Auto. Default
    /// Auto with no configured service leaves Claude's WebSearch available.
    fn claude_tessera_search(&self) -> bool {
        let cfg = self.config.read();
        cfg.web.search != "auto"
            || cfg
                .web
                .searxng_url
                .as_ref()
                .is_some_and(|u| !u.trim().is_empty())
            || crate::tools::web::KEYED
                .iter()
                .any(|p| self.auth.get(&crate::tools::web::key_id(p)).is_some())
    }

    /// Tessera's context for a Claude Code chat (`--append-system-prompt-file`):
    /// its tools in a few lines, memory and the user profile, a bot's persona,
    /// knowledge and contacts, the credentials it may use, the user's own
    /// instructions. Scanned like the native prompt; findings are reported.
    fn claude_context(&self, st: &mut RtState, tools: &[ToolSpec]) -> String {
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let has = |n: &str| names.iter().any(|t| t == n);
        let speaker = self.speaker_bot(st);
        let cfg = self.config.read().clone();
        let mut blocks = Vec::new();
        blocks.push(self.helper_note(st).to_string());
        if !self.redaction_secrets().is_empty() {
            blocks.push("Stored credentials are protected by Tessera. Use mcp__tessera__read, grep, glob, edit, write, bash, job and web tools for content. Native content tools and external MCP servers are disabled while stored values exist. Planning and helpers still use Claude Code's loop.".into());
        }
        let mut findings = Vec::new();
        if names.iter().any(|n| n.starts_with("mcp__")) {
            blocks.push("Tools named `mcp__…` relay the user's Tessera-configured external integrations, with Tessera's permissions.".into());
        }
        let cwd = PathBuf::from(&st.info.cwd);
        let (project, found) =
            crate::prompt::project_context_scanned(&cwd, &self.protected_paths());
        if !project.is_empty() {
            blocks.push(format!("# Project instructions\n{project}"));
        }
        findings.extend(found);
        if has("skill") {
            let (index, found) = crate::skills::prompt_index_scanned(&self.skills_for(&cwd));
            blocks.push(index);
            findings.extend(found);
        }
        // A scheduled duty: nobody watches; it ends with a report.
        if st.info.kind == SessionKind::Job {
            blocks.push(crate::prompt::JOB.to_string());
        }
        if cfg.memory.enabled {
            // A terminal chat: only the user profile, as in Tessera's own.
            let (mut memory, found) = if st.info.lean {
                self.memory_for(speaker.as_ref()).profile_block_scanned()
            } else {
                self.memory_for(speaker.as_ref()).prompt_block_scanned()
            };
            // A bot with its own memory is told which memory is whose.
            if let Some(g) = speaker.as_ref().and_then(crate::bots::memory_guidance) {
                memory.push_str(&g);
            }
            if !memory.trim().is_empty() {
                blocks.push(format!("# Memory\n{memory}"));
            }
            findings.extend(found);
        }
        if let Some(b) = speaker.as_ref().filter(|b| !b.knowledge.is_empty()) {
            let notes: Vec<NoteInfo> = b
                .knowledge
                .iter()
                .filter_map(|id| self.store.note(id).ok().flatten())
                .map(|n| notes::note_info(&n))
                .collect();
            blocks.push(crate::bots::knowledge_block(&notes));
        }
        if has("message_bot") {
            blocks.push(crate::bots::contacts_block(
                speaker.as_ref(),
                &self.all_bots(),
                self.config.read().security.bot_handoff,
            ));
        }
        if has("bash")
            && let Some(secrets) = self.secrets_prompt(st)
        {
            blocks.push(secrets);
        }
        if let (true, Some(b)) = (is_room(&st.info), speaker.as_ref()) {
            let bots = self.all_bots();
            let members: Vec<&Bot> = bots
                .iter()
                .filter(|m| st.info.members.contains(&m.id))
                .collect();
            blocks.push(crate::bots::channel_block(&st.info.title, b, &members));
        }
        if let Some(extra) = cfg.agent.instructions.as_deref()
            && !extra.trim().is_empty()
        {
            blocks.push(format!("# The user's instructions\n{}", extra.trim()));
        }
        self.report_findings(st, findings);
        let persona = speaker.as_ref().map(crate::bots::persona);
        blocks.push(format!(
            "# Environment\n{}",
            crate::prompt::current_environment(&self.effective_model(st))
        ));
        crate::tools::redact_known(
            &cc::context(&names, persona.as_deref(), &blocks),
            &self.redaction_secrets(),
        )
    }

    /// The Claude Code conversation the session (or, in a channel, the bot
    /// speaking now) continues.
    fn claude_link(&self, st: &RtState) -> Option<Link> {
        match st.speaker.as_ref().filter(|_| is_room(&st.info)) {
            Some(bot) => st.internal.claude_code_bots.get(bot).cloned(),
            None => st.internal.claude_code.clone(),
        }
    }

    fn set_claude_link(&self, st: &mut RtState, link: Option<Link>) {
        match st.speaker.clone().filter(|_| is_room(&st.info)) {
            Some(bot) => match link {
                Some(l) => {
                    st.internal.claude_code_bots.insert(bot, l);
                }
                None => {
                    st.internal.claude_code_bots.remove(&bot);
                }
            },
            None => st.internal.claude_code = link,
        }
    }

    /// One turn (and the follow-ups queued meanwhile) in Claude Code. A
    /// plan's usage limit with a known reset is waited out, as in Tessera's
    /// unattended turns, within one bounded wait budget.
    pub(super) async fn claude_code_turn(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        cancel: &CancellationToken,
    ) -> Outcome {
        let mut waits = limits::Waits::default();
        let mut busy = 0;
        // Accounts this turn moved away from (their plan's limit reached).
        let mut tried: Vec<String> = Vec::new();
        loop {
            let mut tries = 0;
            let (outcome, limit) = loop {
                match self.claude_code_attempt(rt, cancel).await {
                    Ok(r) => break r,
                    Err(_) if tries >= 2 => {
                        break (
                            Outcome::Failed(
                                "Claude Code could not continue this chat. Send a message to try again."
                                    .into(),
                            ),
                            None,
                        );
                    }
                    // A conversation Claude Code no longer has: start a new one,
                    // told what happened so far.
                    Err(Retry::Afresh) => {
                        tries += 1;
                        self.set_claude_link(&mut rt.st.lock(), None);
                        self.save(rt);
                    }
                    // The kept process was gone: a new one, same conversation.
                    Err(Retry::Again) => tries += 1,
                }
            };
            // A changed inventory ends a clean run before delivering the next
            // queued turn. Re-launch only here, never in a live tool/turn.
            if matches!(outcome, Outcome::Done) && !cancel.is_cancelled() {
                let next = {
                    let mut st = rt.st.lock();
                    if is_room(&st.info) {
                        None
                    } else {
                        st.take_one(Queue::Steer)
                            .or_else(|| st.take_one(Queue::FollowUp))
                    }
                };
                if let Some(w) = next {
                    let _ = self.append_waiting(rt, w);
                    self.queue_changed(&rt.st.lock());
                    continue;
                }
            }
            // Its sign-in was being renewed by another `claude`: shortly again.
            if let Outcome::Failed(why) = &outcome
                && cc::refreshing(why)
                && busy < 2
                && !cancel.is_cancelled()
            {
                busy += 1;
                self.notice(
                    rt,
                    NoticeLevel::Info,
                    "Another Claude Code on this computer was renewing your sign-in at the same moment; trying again in 30 s.",
                );
                tokio::select! {
                    _ = tokio::time::sleep(REFRESH_WAIT) => continue,
                    _ = cancel.cancelled() => return Outcome::Aborted,
                }
            }
            // The plan's limit is reached: the same model on the user's next
            // Claude account, for good, if they asked for that (`[accounts]`).
            if let (Outcome::Failed(_), Some(_)) = (&outcome, &limit)
                && !cancel.is_cancelled()
            {
                let current = self.effective_model(&rt.st.lock());
                if let Some(next) = self.next_account(&current, &tried) {
                    tried.push(current.provider.clone());
                    if self.move_to_account(rt, &current, &next) {
                        continue;
                    }
                }
            }
            let (Outcome::Failed(_), Some(hit)) = (&outcome, limit) else {
                return outcome;
            };
            if cancel.is_cancelled() {
                return Outcome::Aborted;
            }
            let (info, last_user) = {
                let info = rt.st.lock().info.clone();
                let last_user = self.store.history(&info.id).ok().and_then(|history| {
                    history.iter().rev().find_map(|e| match &e.body {
                        EntryBody::User { author: None, .. } => Some(e.ts),
                        _ => None,
                    })
                });
                (info, last_user)
            };
            let account = self.claude_account(&info.model.provider).key();
            let window = hit.notification_window(&info.id, &account);
            rt.st.lock().claude_limit_window = window.clone();
            let unattended = limits::unattended(&info, last_user, now_ms());
            let Some(wait) = waits.next(&hit, unattended, now_ms(), tokio::time::Instant::now())
            else {
                return Outcome::Failed(hit.notice(false));
            };
            tracing::info!("claude code: waiting {wait:?} for the plan's limit");
            self.notice(rt, NoticeLevel::Warning, hit.notice(true));
            let notify = self.config.read().agent.notify_after_minutes > 0.0
                && !rt.st.lock().claude_limit_notified;
            if notify
                && self.notify_long_work_window(
                    rt,
                    now_ms(),
                    "Waiting for your Claude plan's limit",
                    hit.notice(true),
                    window.as_ref().map(|(id, at)| (id.as_str(), *at)),
                )
            {
                // Not before insertion succeeds. Duplicate persisted windows
                // also suppress this run's terminal error/Finished message.
                rt.st.lock().claude_limit_notified = true;
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = cancel.cancelled() => return Outcome::Aborted,
            }
        }
    }

    /// One `claude` run, with the public plan rejection if it stopped the turn.
    async fn claude_code_attempt(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        cancel: &CancellationToken,
    ) -> Result<(Outcome, Option<limits::Hit>), Retry> {
        // This turn goes on with what its kept process started by itself
        // (taken at once: a run that fails early must not leave it behind).
        let chat_id = rt.st.lock().info.id.clone();
        let resumed = self.claude.resumed.lock().remove(&chat_id);
        if cancel.is_cancelled() {
            return Ok((Outcome::Aborted, None));
        }
        let history_epoch = rt.st.lock().internal.claude_history_epoch;
        let context_epoch = rt.st.lock().internal.claude_context_epoch;
        let (sid, model, cwd, mode, thinking, private, link, room, helper, bot, fast) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                self.effective_model(&st),
                PathBuf::from(&st.info.cwd),
                self.effective_permission(&st),
                st.info.thinking,
                st.info.private,
                self.claude_link(&st),
                is_room(&st.info),
                // Tessera's own helper (the memory review): its tools only.
                st.info.internal && st.tools_only.is_some(),
                self.speaker_bot(&st).filter(|_| !st.info.internal),
                st.info.fast,
            )
        };
        let failed = |msg: String| Ok((Outcome::Failed(msg), None));
        // The Claude account the chat's model is on (its own folder, for any but the usual one).
        let account = self.claude_account(&model.provider);
        if self.providers.claude_code(&account.key()).is_none() {
            self.claude_code_detect(&account).await;
        }
        let Some(path) = cc::locate(&account.command) else {
            self.claude_code_known(&account.key(), cc::Detection::default());
            return failed(
                cc::info(
                    &cc::Detection::default(),
                    &crate::node::hostname(),
                    &model.provider,
                    &account,
                )
                .hint,
            );
        };
        if let Err(e) = self.refresh_environment(rt) {
            return failed(e.to_string());
        }
        let entries = match self.store.history(&sid) {
            Ok(e) => e,
            Err(e) => return failed(e.to_string()),
        };
        // Continue Claude Code's conversation only if it is this chat's, in
        // the same folder (it keeps them by folder) and on the same account
        // (each keeps its own), and nothing it saw was rewound since. A
        // helper starts afresh each time.
        let link = link.filter(|l| {
            !helper
                && l.owner == sid
                && l.dir == dir_key(&cwd)
                && l.on_account(&model.provider)
                && entries.iter().any(|e| e.seq == l.seq && e.ts == l.ts)
        });
        let seen = link.as_ref().map_or(0, |l| l.seq);
        // Added to the chat since Claude Code last saw it (a helper's report,
        // a note): never skipped, even when it carries on by itself.
        let unseen = entries
            .iter()
            .any(|e| e.seq > seen && !matches!(e.body, EntryBody::Notice { .. }));
        let mut prompt = match bot.as_ref().filter(|_| room) {
            Some(b) => {
                let bots = self.all_bots();
                let handle = |id: &str| {
                    bots.iter()
                        .find(|x| x.id == id)
                        .map(|x| format!("@{}", x.handle))
                        .unwrap_or_else(|| "a bot".into())
                };
                room_prompt(&entries, seen, &b.id, &handle)
            }
            None => {
                // Who wrote what it hasn't seen (a bot, another chat's
                // agent), as Tessera's own turns label it; harness notes
                // say what they are.
                let labels = self.author_labels(&entries, &rt.st.lock());
                let label = |a: &str| match a {
                    "harness" => String::new(),
                    a => labels.get(a).cloned().unwrap_or_default(),
                };
                prompt(&entries, seen, &label)
            }
        };
        // Private chats keep nothing on disk: a scratch folder, no history.
        let dir = if private {
            let d = std::env::temp_dir().join(format!("tessera-private-{sid}"));
            if let Err(e) = std::fs::create_dir_all(&d) {
                return failed(format!("Could not make a scratch folder: {e}"));
            }
            d
        } else {
            cwd.clone()
        };
        // A folder that is gone (or can't be opened) would read as Claude Code
        // failing to start: say what it is.
        if !dir.is_dir() {
            return failed(format!(
                "This chat's folder ({}) doesn't exist or can't be opened. Choose another folder for this chat, then send your message again.",
                dir.display()
            ));
        }
        let directory = match self.process_directory(rt, &dir) {
            Ok(d) => d,
            Err(e) => return failed(e),
        };
        // Tessera's context is absent in private chats (search can still be bridged).
        let (tools, context) = {
            let mut st = rt.st.lock();
            // The model's own window from the start of the turn (an older Tessera could
            // have stored another model's).
            if let Some(i) = self.providers.model_info(&model) {
                st.info.context.window = i.context_window;
            }
            let tools = self.claude_bridge_tools(&st);
            // The context its conversation started with, while the tools are
            // the same (see `Link::context`).
            let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            let context = (!private && !helper).then(|| {
                link.as_ref()
                    .and_then(|l| l.context.clone())
                    .filter(|c| c.tools == names || st.internal.claude_secure_entry)
                    .map(|mut c| {
                        c.tools = names.clone();
                        c
                    })
                    .unwrap_or_else(|| cc::FrozenContext {
                        text: self.claude_context(&mut st, &tools),
                        tools: names,
                    })
            });
            (tools, context)
        };
        let (ledger_version, ledger_update) = ledger_context(rt.st.lock().info.task.as_ref());
        if !private && !helper {
            let update = if link
                .as_ref()
                .is_some_and(|l| l.ledger_version == ledger_version)
            {
                crate::tools::meta::unchanged_ledger(&ledger_version)
            } else {
                ledger_update
            };
            prompt.insert(0, ContentPart::text(update));
        }
        let secure_entry = rt.st.lock().internal.claude_secure_entry;
        if secure_entry && let Some(credentials) = self.secrets_prompt(&rt.st.lock()) {
            // Metadata only. The original appended context stays frozen;
            // freshly saved variable names arrive as normal turn context.
            prompt.insert(0, ContentPart::text(format!("[Tessera password library updated; values were entered securely, not in chat.]\n{credentials}")));
        }
        let limits = {
            let st = rt.st.lock();
            let mut limits = cc::limits_for(Some(&self.claude_capabilities(&st)));
            if st.depth > 0 {
                limits.only.retain(|tool| *tool != "Workflow");
                limits.deny.push("Workflow");
            }
            limits.own_mcp_off = !self.claude_external_allowed(&st, None);
            if self.claude_tessera_search() {
                limits.only.retain(|tool| *tool != "WebSearch");
                limits.deny.push("WebSearch");
            }
            if !self.redaction_secrets().is_empty() {
                limits.protect_secrets();
            }
            limits
        };
        // The command sandbox, as for the agent's commands: Tessera's data and
        // credentials hidden, `~/.claude` and the network left as they are.
        let sandbox = self.session_sandbox(&sid);
        let (verify, keep_open) = {
            let cfg = self.config.read();
            (
                cfg.agent.verify_check != VerifyCheck::Off,
                cfg.agent.claude_keep_open_minutes,
            )
        };
        // Tessera's hooks: copies of files before Claude Code changes them (for
        // rewind) and the verification note. Not in private chats (nothing is
        // kept) or Tessera's own helpers (no tools of Claude Code's).
        let hooks = !private && !helper;
        let fast = fast && !helper && self.providers.model_info(&model).is_some_and(|i| i.fast);
        let uploads = self.paths.uploads_dir().join(&sid);
        let mut launch = cc::Launch {
            model: &model.model,
            effort: cc::effort(thinking),
            fast,
            mode,
            resume: None,
            private,
            add_dirs: uploads.is_dir().then_some(uploads).into_iter().collect(),
            bridge: !tools.is_empty(),
            context: None,
            limits,
            helper,
        };
        // A process kept open after an earlier turn takes this one only if it
        // would be started exactly the same way now: same flags, folder,
        // sandbox, context and hooks, same Claude Code conversation.
        let key = format!(
            "{}\u{1e}{}\u{1e}{}\u{1e}{:?}\u{1e}{}\u{1e}{}\u{1e}{}\u{1e}{}",
            account.key(),
            cc::args(&launch).join("\u{1f}"),
            dir.display(),
            sandbox,
            context.as_ref().map_or("", |c| c.text.as_str()),
            hooks && verify,
            serde_json::to_string(&tools).expect("tool specs serialize"),
            context_epoch,
        );
        let (mut carried, mut carried_closed) = (false, false);
        let kept = match self.claude.take(&sid) {
            Some(p)
                if link.as_ref().is_some_and(|l| l.session == p.session)
                    && (p.key == key
                        || (secure_entry && same_launch_with_new_tool_schemas(&p.key, &key))) =>
            {
                carried = p.carried();
                p.resume().await
            }
            // Started another way than it would be now (settings changed):
            // never continue under the old ones, even what it started itself.
            Some(p) => {
                carried_closed = p.carried();
                p.close().await;
                None
            }
            None => None,
        };
        // It is in the middle of a turn of its own: its lines so far come first.
        let adopted = carried && kept.is_some();
        if resumed && !adopted {
            if !carried_closed {
                // Its process is gone (it ended, or a Stop closed it): nothing
                // goes on that nobody asked for.
                self.notice(
                    rt,
                    NoticeLevel::Info,
                    "Claude Code started working again by itself, but that run had ended. Send a message to continue.",
                );
                return Ok((Outcome::Done, None));
            }
            self.notice(
                rt,
                NoticeLevel::Info,
                "Claude Code started working again by itself, but this chat's settings changed meanwhile: it goes on in a new run, with the current settings.",
            );
        }
        let warm = kept.is_some();
        rt.st.lock().internal.claude_secure_entry = false;
        let mut proc = match kept {
            Some(p) => p,
            None => {
                let scratch = match context
                    .as_ref()
                    .map(|c| RunFiles::with_context(&c.text))
                    .transpose()
                {
                    Ok(s) => s,
                    Err(e) => return failed(format!("Could not prepare Claude Code's run: {e}")),
                };
                launch.resume = link.as_ref().map(|l| l.session.as_str());
                launch.context = scratch.as_ref().map(|s| s.context.as_path());
                let args = cc::args(&launch);
                let init = cc::initialize_line("init", hooks, verify);
                match Proc::spawn(
                    &path,
                    &account,
                    args,
                    directory,
                    sandbox.as_ref(),
                    scratch,
                    init,
                    &self.config.read().agent.pass_env,
                    private,
                ) {
                    Ok(p) => p,
                    Err(e) => return failed(e),
                }
            }
        };
        let wtx = proc.wtx.take().expect("a process has its input");
        // No user work enters a fresh CLI until it accepts the hook registration.
        let mut initial_prompt = Some(prompt);
        let init_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);

        let author = self.speaker_id(&rt.st.lock());
        let mut t = Turn::new(sid.clone(), model.clone(), dir.clone(), mode, author);
        t.command = account.key();
        t.first = link.is_none();
        t.room = room;
        t.fast = fast;
        t.guard = self.guardrails(rt);
        t.turn_from = entries
            .iter()
            .rev()
            .find(|e| matches!(e.body, EntryBody::User { .. }))
            .map_or(0, |e| e.seq.saturating_sub(1));
        t.ledger_version = link
            .as_ref()
            .map(|l| l.ledger_version.clone())
            .unwrap_or_default();
        t.last = link.as_ref().map(|l| (l.seq, l.ts));
        let initial_boundary = entries.last().map(|e| (e.seq, e.ts));
        let initial_ledger = (!private && !helper).then_some(ledger_version);
        if adopted {
            // A turn of its own (it started it): nothing to send unless a
            // message started this turn or something reached the chat since
            // its last turn; it takes that up after its own.
            t.adopted = true;
            t.started = true;
            t.claude_session = link.as_ref().map(|l| l.session.clone());
            if resumed && !unseen {
                initial_prompt = None;
            }
        }
        if warm && let Some(prompt) = initial_prompt.take() {
            let _ = t.send_input(&wtx, &prompt, initial_ledger.clone(), initial_boundary);
        }
        t.snapshots = hooks;
        t.cancel = cancel.clone();
        if warm {
            tracing::debug!(
                "session {sid}: Claude Code's process from the last turn takes this one"
            );
        }
        t.reported = link
            .as_ref()
            .map(|l| l.reported.clone())
            .unwrap_or_default();
        if !tools.is_empty() {
            t.bridge = Some(Bridge {
                tools,
                done: Default::default(),
                cancel: cancel.child_token(),
            });
        }
        let inventory = serde_json::to_string(&self.claude_bridge_tools(&rt.st.lock()))
            .expect("tool specs serialize");
        let helpers = self.helpers_offered(&rt.st.lock());
        let helpers_changed = || helpers != self.helpers_offered(&rt.st.lock());
        let context_changed = || {
            let st = rt.st.lock();
            context_epoch != st.internal.claude_context_epoch
                || history_epoch != st.internal.claude_history_epoch
        };
        let stale = || {
            let st = rt.st.lock();
            context_epoch != st.internal.claude_context_epoch
                || history_epoch != st.internal.claude_history_epoch
                || (!st.internal.claude_secure_entry
                    && inventory
                        != serde_json::to_string(&self.claude_bridge_tools(&st))
                            .expect("tool specs serialize"))
        };
        let mut wtx = Some(wtx);
        let mut refreshing = false;
        let mut stopping = false;
        // The turn ended cleanly and the process stays open for the next one.
        let mut parking = false;
        let can_park = |t: &Turn| {
            keep_open > 0
                && !private
                && !helper
                && !room
                && t.finished
                && t.failure.is_none()
                && !t.retain_background()
                && t.taken >= t.sent
                && t.claude_session.is_some()
        };
        let deadline = tokio::time::sleep(STOP_GRACE);
        tokio::pin!(deadline);
        // The next queued message (none in a channel: that starts a new round).
        let next_queued = |t: &Turn| {
            if t.failure.is_some() || room {
                return None;
            }
            let mut st = rt.st.lock();
            st.take_one(Queue::Steer)
                .or_else(|| st.take_one(Queue::FollowUp))
        };
        let deliver = |t: &mut Turn, tx: &mpsc::UnboundedSender<String>, w: Waiting| {
            let (version, update) = ledger_context(rt.st.lock().info.task.as_ref());
            let mut content = w.content.clone();
            if !private && !helper {
                let update = if t.inputs_confirmed() && version == t.ledger_version {
                    crate::tools::meta::unchanged_ledger(&version)
                } else {
                    update
                };
                content.insert(0, ContentPart::text(update));
            }
            if w.author.is_none() {
                t.nudged = false;
                t.guard.new_turn();
                t.guard_notes.clear();
                t.turn_from = rt.st.lock().info.last_seq + 1;
            }
            let own_report = w
                .author
                .as_deref()
                .and_then(|a| a.strip_prefix("subagent:"))
                .and_then(|child| self.store.get_session(child).ok().flatten())
                .is_some_and(|(info, internal)| {
                    info.parent_id.as_deref() == Some(sid.as_str()) && internal.subagent.is_some()
                });
            let entry = match self.append_waiting(rt, w) {
                Ok(e) => e,
                Err(e) => {
                    t.failure = Some(format!("Could not save queued message: {e}"));
                    return;
                }
            };
            // Saving/queueing is not acceptance. Keep the previous resume
            // boundary until the exact input is echoed as taken by the CLI.
            if t.send_input(
                tx,
                &content,
                (!private && !helper).then_some(version),
                Some((entry.seq, entry.ts)),
            ) && own_report
            {
                // Renew only after this admitted report's exact acceptance,
                // never merely because it was saved or sent to stdin.
                t.pending_inputs
                    .back_mut()
                    .expect("sent input")
                    .completion_dependency = true;
            }
            let _ = self.events.send(Event::QueueChanged {
                session_id: sid.clone(),
                queued: queued_messages(&rt.st.lock()),
            });
        };
        // Its turn ended with work left running in the background.
        let mut waiting = BackgroundWait::default();
        // It went on after its input closed (noticed once).
        let mut carried_on = false;
        let mut heard = tokio::time::Instant::now();
        // Lines read, and the last few that weren't its JSON (an error printed
        // as plain text says why a run ended, when nothing else does).
        let (mut lines_in, mut stray) = (0usize, Vec::<String>::new());
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut changes = self.events.subscribe();
        loop {
            // Claude's --tools is fixed at launch. Refresh at an action
            // boundary, never interrupt a running tool or background task.
            if !stopping
                && !refreshing
                && helpers_changed()
                && t.started
                && t.open.is_empty()
                && t.approvals.is_empty()
                && !t.retain_background()
                && t.taken >= t.sent
            {
                if t.finished {
                    break;
                }
                refreshing = true;
                if let Some(tx) = &wtx {
                    let _ = tx.send(cc::control_line("refresh-tools", "interrupt"));
                }
                deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + STOP_GRACE);
            }
            tokio::select! {
                _ = tokio::time::sleep_until(init_deadline), if initial_prompt.is_some() => {
                    t.failure = Some("Claude Code did not accept Tessera's permission hooks; no user work was sent.".into());
                    break;
                }
                _ = changes.recv(), if !stopping && !refreshing => {
                    self.claude_sync_permission(rt, &mut t, wtx.as_ref());
                }
                line = proc.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    heard = tokio::time::Instant::now();
                    lines_in += 1;
                    let Ok(v) = serde_json::from_str::<Value>(&line) else {
                        let text = line.trim();
                        if !text.is_empty() {
                            tracing::debug!("session {sid}: Claude Code wrote a line that isn't JSON: {}", crate::util::truncate(text, 200));
                            stray.push(crate::util::truncate(text, 300));
                            if stray.len() > 6 {
                                stray.remove(0);
                            }
                        }
                        continue;
                    };
                    if waiting.heard(&v) {
                        // Busy again, in a turn of its own: the chat shows it,
                        // and it ends with its result like any turn.
                        t.finished = false;
                        self.set_status(rt, SessionStatus::Running);
                    }
                    if wtx.is_none() && !stopping && !carried_on && BackgroundWait::takes_turn(&v) {
                        // Too late to keep its input open: say so rather than let its
                        // approvals and Tessera's tools fail without a word.
                        carried_on = true;
                        tracing::warn!("session {sid}: Claude Code carried on after its input was closed");
                        self.notice(
                            rt,
                            NoticeLevel::Warning,
                            "Claude Code carried on by itself after Tessera stopped waiting for its background work: in this part it can't ask for approvals or use Tessera's tools. Send a message to continue properly.",
                        );
                    }
                    if initial_prompt.is_some() && v.get("type").and_then(Value::as_str) == Some("control_response")
                        && v.pointer("/response/request_id").and_then(Value::as_str) == Some("init") {
                        if v.pointer("/response/subtype").and_then(Value::as_str) != Some("success") {
                            t.failure = Some("Claude Code refused Tessera's permission hooks; no user work was sent.".into());
                            break;
                        }
                        if let (Some(prompt), Some(tx)) = (initial_prompt.take(), wtx.as_ref())
                            && !t.send_input(tx, &prompt, initial_ledger.clone(), initial_boundary)
                        {
                            break;
                        }
                    }
                    self.claude_sync_permission(rt, &mut t, wtx.as_ref());
                    if refreshing && v.get("type").and_then(Value::as_str) == Some("control_request") {
                        if let Some(tx) = wtx.as_ref() {
                            let _ = tx.send(cc::decline_request(&v, "Not performed: this chat's tools changed. Claude Code restarts with the updated list and continues; submit the action again if it is still needed."));
                        }
                        continue;
                    }
                    let done = self.claude_message(rt, &mut t, v, wtx.as_ref(), cancel);
                    if refreshing && done { break; }
                    if !done {
                        continue;
                    }
                    // Exact context refresh is bounded for older CLIs.
                    let mut went_on = false;
                    if let Some(tx) = wtx.as_ref().filter(|_| !t.retain_background() && t.taken >= t.sent) {
                        self.claude_context_request(&mut t, tx, true);
                        let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
                        while t.context_request.is_some() {
                            let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline, proc.next_line()).await else { break };
                            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                            // No new work is admitted after the turn ended.
                            // Especially not late MCP calls during this wait.
                            if v.get("type").and_then(Value::as_str) == Some("control_response") {
                                self.claude_message(rt, &mut t, v, wtx.as_ref(), cancel);
                            } else if v.get("type").and_then(Value::as_str) == Some("control_request") {
                                let _ = tx.send(cc::decline_request(&v, "Not performed: this turn had already finished. Submit the action again in your next turn if it is still needed."));
                            } else {
                                // Anything else is read as usual, in order: a turn it
                                // took up by itself right away goes on in this run.
                                went_on = BackgroundWait::answers(&v);
                                proc.backlog.push_back(line);
                                break;
                            }
                        }
                    }
                    if went_on {
                        t.finished = false;
                        continue;
                    }
                    // Its turn ended: the next queued message, or (with nothing
                    // left in the background) close stdin.
                    // (With its input closed, a queued message waits for the next run.)
                    if !stopping && t.finished && t.failure.is_none() && !t.retain_background() && t.taken >= t.sent && stale() {
                        break;
                    }
                    let next = if stopping || wtx.is_none() || context_changed() || rt.st.lock().internal.claude_secure_entry { None } else { next_queued(&t) };
                    match (next, wtx.as_ref()) {
                        (Some(w), Some(tx)) => {
                            deliver(&mut t, tx, w);
                            waiting.stop();
                        }
                        // A command or subagent it runs in the background goes on, and
                        // Claude Code takes the turn up again when it is done; or that
                        // turn was one of its own (after a restart, say) and a message
                        // sent to it is still to come. Until then it keeps its input
                        // (approvals, Tessera's tools, messages).
                        _ if wtx.is_some()
                            && !stopping
                            && t.failure.is_none()
                            && (t.retain_background() || t.taken < t.sent) =>
                        {
                            waiting.start();
                            // Its answer is in: the chat is idle (messages go
                            // straight to it) while the work goes on.
                            if t.retain_background() {
                                self.set_status(rt, SessionStatus::Idle);
                                self.claude_wait_notice(rt, &mut waiting, t.background_count(), t.retain_background());
                            }
                        }
                        // All done: the process stays open for the chat's next
                        // turn (see `ClaudeProcs`), or its input closes.
                        _ if wtx.is_some() && !stopping && can_park(&t) => {
                            parking = true;
                            break;
                        }
                        _ => {
                            self.claude_close(&mut t);
                            wtx = None;
                            waiting.stop();
                        }
                    }
                }
                // Streamed pieces that waited long enough.
                _ = tokio::time::sleep_until(t.deltas.due().unwrap_or_else(tokio::time::Instant::now)), if t.deltas.due().is_some() => {
                    self.send_deltas(rt, &mut t);
                }
                _ = tick.tick(), if !stopping && !refreshing => {
                    self.check_budget(rt);
                    // Subagents the user stopped alone.
                    let stops = self.claude.stops.lock().remove(&sid).unwrap_or_default();
                    for call in stops {
                        self.claude_stop_subagent_now(&mut t, &call, wtx.as_ref());
                    }
                    // Expiry is independent of the frozen context. Closing input
                    // also starts the bounded process cleanup when settings changed.
                    let background_tick = waiting.tick(context_changed() || rt.st.lock().internal.claude_secure_entry);
                    if background_tick == BackgroundTick::Expired {
                        tracing::info!("session {sid}: stopped waiting for Claude Code's background work");
                        self.notice(rt, NoticeLevel::Warning, format!(
                            "Stopped waiting on Claude Code's background work after 12 hours ({} still reported running). Send a message to continue.", t.background_count()));
                        self.claude_close(&mut t);
                        wtx = None;
                        waiting.stop();
                        break;
                    }
                    if waiting.waiting() {
                        self.claude_wait_notice(rt, &mut waiting, t.background_count(), t.retain_background());
                    }
                    // Mid-turn, steering goes in at once: Claude Code takes it up
                    // at its next step, in this turn. Follow-ups wait for its end.
                    if !waiting.waiting() && !context_changed() && !rt.st.lock().internal.claude_secure_entry {
                        let steer = if room || t.finished {
                            None
                        } else {
                            let mut st = rt.st.lock();
                            st.take_one(Queue::Steer)
                        };
                        if let (Some(tx), Some(w)) = (wtx.as_ref(), steer) {
                            deliver(&mut t, tx, w);
                        }
                        continue;
                    }
                    // Changed settings wait for the current turn to finish;
                    // never deliver queued work using its frozen inventory.
                    if background_tick == BackgroundTick::Stale {
                        if t.finished && !t.retain_background() && t.taken >= t.sent && t.taken <= t.answered {
                            parking = !context_changed() && can_park(&t);
                            break;
                        }
                        continue;
                    }
                    // A message sent meanwhile goes in now.
                    if let (Some(tx), Some(w)) = (wtx.as_ref(), next_queued(&t)) {
                        deliver(&mut t, tx, w);
                        waiting.stop();
                        self.set_status(rt, SessionStatus::Running);
                        continue;
                    }
                    let quiet = gone_quiet(t.retain_background(), heard.elapsed(), t.carry_on_expected);
                    if quiet && wtx.is_some() && can_park(&t) {
                        parking = true;
                        break;
                    }
                    if quiet || waiting.expired() {
                        if !quiet {
                            tracing::info!(
                                "session {sid}: stopped waiting for Claude Code's background work after {} h",
                                BACKGROUND_MAX.as_secs() / 3600
                            );
                        }
                        self.claude_close(&mut t);
                        wtx = None;
                        waiting.stop();
                    }
                }
                _ = cancel.cancelled(), if !stopping => {
                    // Private chats keep no helper sessions for Abort to list.
                    if private {
                        let helpers: Vec<String> = t.agents.values()
                            .filter(|a| a.started && !a.done)
                            .map(|a| one_line(&a.description, 80))
                            .collect();
                        if !helpers.is_empty() {
                            self.notice(rt, NoticeLevel::Info, format!(
                                "Stopping this chat, including {} {}: {}.",
                                helpers.len(),
                                if helpers.len() == 1 { "helper" } else { "helpers" },
                                helpers.join(", ")
                            ));
                        }
                    }
                    stopping = true;
                    if let Some(tx) = &wtx {
                        let _ = tx.send(cc::control_line("stop", "interrupt"));
                    }
                    deadline.as_mut().reset(tokio::time::Instant::now() + STOP_GRACE);
                }
                _ = &mut deadline, if stopping || refreshing => break,
            }
        }
        for (_, a) in t.approvals.drain() {
            a.cancel();
        }
        if let Some(b) = &t.bridge {
            b.cancel.cancel();
        }
        // Copies taken before its edits: what the files hold now.
        for seq in std::mem::take(&mut t.snapped) {
            let _ = self.seal_snapshots(&sid, seq);
        }
        let (kept, ended) = if parking {
            proc.wtx = wtx;
            (Some(proc), Ended::default())
        } else {
            drop(wtx);
            (None, proc.close().await)
        };
        if private {
            let _ = std::fs::remove_dir_all(&dir);
        }
        if stopping || !t.finished {
            self.claude_cut_off(rt, &mut t, stopping);
        }
        // A whole message it hadn't followed up yet (it ended right after) stays too.
        self.claude_flush(rt, &mut t);
        self.end_stream(rt, &mut t);
        // Calls that never got a result (stopped, or it died).
        for (id, name) in std::mem::take(&mut t.open) {
            let text = if stopping {
                "Stopped by the user before this finished."
            } else {
                "Claude Code ended before this finished."
            };
            let _ = self.store.tool_finished(&sid, &id);
            let _ = self.append(
                rt,
                EntryBody::ToolResult {
                    call_id: id,
                    tool: name,
                    content: vec![ContentPart::text(text)],
                    is_error: true,
                    artifact: None,
                    details: None,
                },
            );
        }
        // Subagents still at work (in the background, or the run stopped) end with it.
        let model = t.resolved.clone().unwrap_or_else(|| t.model.model.clone());
        let why = if stopping {
            "Stopped with the rest of the turn."
        } else {
            "Claude Code's run ended before this subagent finished."
        };
        for a in t.agents.values_mut() {
            self.claude_agent_end(
                a,
                &t.model.provider,
                &model,
                SessionStatus::Interrupted,
                Some(why),
            );
        }
        {
            let mut st = rt.st.lock();
            if let Some(s) = st.streaming.as_mut() {
                s.running_tools.clear();
            }
        }
        // Link writes and secure-entry invalidation share the same state lock;
        // a turn finishing concurrently cannot restore an unsanitized resume.
        let linked = {
            let mut st = rt.st.lock();
            if !private && !helper && history_epoch == st.internal.claude_history_epoch {
                if let (Some(session), Some((seq, ts))) = (t.claude_session.clone(), t.last) {
                    let link = Link {
                        session,
                        owner: sid.clone(),
                        provider: t.model.provider.clone(),
                        dir: dir_key(&cwd),
                        seq,
                        ts,
                        reported: std::mem::take(&mut t.reported),
                        ledger_version: t.delivered_ledger_version(),
                        // After compaction/settings changes, rebuild context.
                        context: context.filter(|_| {
                            !t.compacted && context_epoch == st.internal.claude_context_epoch
                        }),
                    };
                    self.set_claude_link(&mut st, Some(link));
                    true
                } else if t.compacted
                    && let Some(l) = self.claude_link(&st)
                {
                    self.set_claude_link(&mut st, Some(Link { context: None, ..l }));
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if linked {
            self.save(rt);
        }
        if stopping {
            return Ok((Outcome::Aborted, None));
        }
        if refreshing {
            let note = self.helper_note(&rt.st.lock());
            self.append(rt, EntryBody::User {
                content: vec![ContentPart::text(format!("{note} Continue the current task with the updated tools; don't repeat completed actions."))],
                author: Some("harness".into()),
            }).map_err(|_| Retry::Again)?;
            return Err(Retry::Again);
        }
        // The process kept from the last turn had ended before it took this
        // one (it said nothing at all): a new process, same conversation.
        if warm && lines_in == 0 && !t.finished && !stopping {
            tracing::info!(
                "session {sid}: the kept Claude Code process had ended; starting a new one"
            );
            return Err(Retry::Again);
        }
        // It couldn't resume (its copy is gone or unusable): start afresh. Not
        // when it never got that far (no output at all, and its error isn't
        // about the conversation: an option it doesn't know, a crash at start).
        let reached = lines_in > 0 || ended.stderr.to_ascii_lowercase().contains("conversation");
        if link.is_some() && !t.started && reached && (t.failure.is_some() || !t.finished) {
            return Err(Retry::Afresh);
        }
        if let Some(f) = t.failure {
            let hit = t.limit.or_else(|| limits::Hit::result(&f));
            let (msg, signed_out) = cc::failure(&f);
            if signed_out && hit.is_none() {
                self.claude_code_signed_in(&t.command, false);
            }
            return Ok((Outcome::Failed(msg), hit));
        }
        if !t.finished {
            let version = self
                .providers
                .claude_code(&t.command)
                .and_then(|d| d.version);
            return failed(cc::unfinished(
                &ended.stderr,
                &stray.join("\n"),
                ended.how.as_deref(),
                version.as_deref(),
            ));
        }
        self.claude_code_signed_in(&t.command, true);
        if let (Some(p), Some(session)) = (kept, t.claude_session.clone()) {
            if history_epoch != rt.st.lock().internal.claude_history_epoch {
                p.close().await;
                return Ok((Outcome::Done, None));
            }
            self.claude_park(&sid, p, key, session, keep_open);
        }
        Ok((Outcome::Done, None))
    }

    /// The mandatory policy hook authorizes every action; only then does its
    /// file-copy bookkeeping run before the CLI is released. A separate edit
    /// callback would race approvals because CLI hooks may run concurrently.
    /// Stop observes lifecycle and optionally nudges verification.
    fn claude_hook(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        request_id: String,
        req: &Value,
        wtx: Option<&mpsc::UnboundedSender<String>>,
    ) {
        let Some(tx) = wtx.cloned() else { return };
        let input = req.get("input").unwrap_or(&Value::Null);
        let carry_on = |tx: &mpsc::UnboundedSender<String>, id: &str| {
            let _ = tx.send(cc::hook_line(id, json!({})));
        };
        match req.get("callback_id").and_then(Value::as_str) {
            Some(cc::HOOK_POLICY) => {
                let policy_req = json!({
                    "tool_name": input.get("tool_name"),
                    "input": input.get("tool_input"),
                    "tool_use_id": req.get("tool_use_id").or_else(|| input.get("tool_use_id")),
                    "agent_id": input.get("agent_id"),
                    "agent_type": input.get("agent_type"),
                    "permission_mode": input.get("permission_mode"),
                    "cwd": input.get("cwd"),
                    "mcp_server": input.get("mcp_server"),
                });
                self.claude_gate(rt, t, request_id, policy_req, tx, true);
            }
            Some(cc::HOOK_CHECK) => {
                let mut args = input.get("tool_input").cloned().unwrap_or(Value::Null);
                if let Some(cwd) = input.get("cwd").and_then(Value::as_str) {
                    args["cwd"] = json!(cwd);
                    if let Some(path) = cc::edit_path(&args) {
                        args["path"] = json!(Path::new(cwd).join(path));
                    }
                }
                let name = input
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let Some(path) = args
                    .get("path")
                    .and_then(Value::as_str)
                    .or_else(|| cc::edit_path(&args))
                    .filter(|_| cc::is_edit_tool(&name))
                else {
                    return carry_on(&tx, &request_id);
                };
                let path = t.cwd.join(path);
                let me = self.clone();
                let rt = rt.clone();
                let checked = t.post_checks.clone();
                let call = req
                    .get("tool_use_id")
                    .or_else(|| input.get("tool_use_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let cancel = t.cancel.clone();
                let cwd = args
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| t.cwd.clone());
                tokio::spawn(async move {
                    let (session_id, admission) = {
                        let st = rt.st.lock();
                        (st.info.id.clone(), me.action_admission(&st))
                    };
                    let ctx = ToolCtx {
                        session_id,
                        call_id: String::new(),
                        seq: 0,
                        cwd,
                        cancel,
                        host: Arc::new(Host(me.clone(), false)),
                        progress: Arc::new(|_| {}),
                        depth: 0,
                        cleared: None,
                        admission,
                    };
                    let mut note = String::new();
                    // Reassess before reading a CLI-owned path after the edit.
                    if !ctx.cancel.is_cancelled()
                        && !matches!(
                            me.claude_policy(&rt, &ctx.cwd, &name, &args, &json!({})),
                            Policy::Deny(_)
                        )
                        && let Ok(bytes) =
                            crate::tools::files::authorized_bytes(&path, &ctx, MAX_SNAPSHOT).await
                        && let Ok(text) = String::from_utf8(bytes)
                    {
                        let display = ctx.display(&path);
                        let after =
                            crate::tools::files::post_write(&ctx, &path, &display, None, &text)
                                .await;
                        if let Some(clean) = after.clean {
                            let out = ToolOutput::text("").with_details(
                                json!({"checked": if clean { "clean" } else { "problems" }}),
                            );
                            checked.lock().insert(
                                call,
                                (json!({"action": "diagnostics", "path": path}), out),
                            );
                        }
                        note = after.blocks.join("\n");
                        if note.is_empty() && after.clean == Some(true) {
                            note = format!("[File check] {display}: clean.");
                        }
                    }
                    let _ = tx.send(cc::hook_line(&request_id, if note.is_empty() { json!({}) } else {
                        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": note}})
                    }));
                });
            }
            Some(cc::HOOK_STOP) => {
                let again = input.get("stop_hook_active").and_then(Value::as_bool) == Some(true);
                // A child finishing is not the parent finishing. A relevant
                // dependency result invalidates the cached reasons separately.
                let child = input.get("agent_id").is_some_and(|a| !a.is_null());
                // Commit before checking so an acknowledgement updates skip state.
                if !child {
                    if let Some(crons) = input.get("session_crons").and_then(Value::as_array) {
                        t.carry_on_expected |= t.finished && t.crons > 0 && crons.is_empty();
                        t.crons = crons.len();
                        self.claude_dependencies(rt, t);
                    }
                    self.claude_flush(rt, t);
                }
                let mut notes = Vec::new();
                if !again && !child && !t.nudged && !t.cancel.is_cancelled() {
                    if let Some(note) = self.verify_note(rt, &mut t.guard, t.turn_from) {
                        notes.push(note);
                    }
                    if let Some(note) = self.completion_check(rt) {
                        notes.push(note);
                    }
                    notes.append(&mut t.guard_notes);
                }
                if notes.is_empty() {
                    carry_on(&tx, &request_id);
                } else {
                    t.nudged = true;
                    let out = cc::stop_output(&notes.join("\n\n"));
                    let _ = tx.send(cc::hook_line(&request_id, out));
                }
            }
            _ => carry_on(&tx, &request_id),
        }
    }

    /// The transcript entry a tool call belongs to: its own, or for a
    /// subagent's call the entry of the `Agent` call that started it.
    fn claude_call_seq(&self, rt: &Rt, t: &Turn, call: &str) -> u64 {
        t.calls
            .get(call)
            .copied()
            .or_else(|| {
                t.agents
                    .iter()
                    .find(|(_, a)| a.open.iter().any(|(id, _)| id == call))
                    .and_then(|(c, _)| t.calls.get(c).copied())
            })
            .unwrap_or_else(|| rt.st.lock().info.last_seq)
    }

    /// A tool call of its own or of one of its subagents finished: what its
    /// edits and checks mean for the verification note, and the copy taken
    /// before an edit gets what the file holds now.
    fn claude_tool_done(self: &Arc<Self>, t: &mut Turn, call: &str, _name: &str, _failed: bool) {
        let snapped = t.edits.remove(call);
        if let Some((_, seq)) = &snapped {
            let _ = self.seal_snapshots(&t.sid, *seq);
        }
        t.edit_paths.remove(call);
    }

    /// CLI task tools only alter the plan inside the same durable ledger.
    fn claude_task_result(&self, rt: &Rt, name: &str, args: &Value, message: &Value) {
        if !matches!(name, "TaskCreate" | "TaskUpdate" | "TodoWrite") {
            return;
        }
        let changed = {
            let mut st = rt.st.lock();
            let RtState { info, internal, .. } = &mut *st;
            let ledger = info.task.get_or_insert_with(TaskLedger::default);
            cc::task_result(
                &mut ledger.plan,
                &mut internal.claude_tasks,
                name,
                args,
                message.get("tool_use_result").unwrap_or(&Value::Null),
            )
        };
        if changed {
            self.save(rt);
        }
    }

    /// Use native observational bookkeeping, without replacing Claude's loop.
    fn claude_observe(&self, t: &mut Turn, call: &str, name: &str, out: &ToolOutput) {
        let Some(mut args) = t.call_args.remove(call) else {
            return;
        };
        let name = match name {
            "Bash" | "PowerShell" => "bash",
            "Write" => "write",
            "Read" => "read",
            "Glob" => "glob",
            "Grep" => "grep",
            "WebFetch" => "web_fetch",
            "WebSearch" => "web_search",
            n if cc::is_edit_tool(n) => "edit",
            n => n,
        };
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| cc::edit_path(&args))
            .map(str::to_string);
        if let Some(path) = path {
            args["path"] = json!(path);
        }
        if let Some(note) = t.guard.observe(name, &args, out)
            && t.guard_notes.len() < 3
            && !t.guard_notes.contains(&note)
        {
            t.guard_notes.push(note);
        }
        if let Some((args, out)) = t.post_checks.lock().remove(call) {
            t.guard.verify.observe("lsp", &args, &out);
        }
    }

    /// The user stopped one of Claude Code's subagents (its session): its
    /// chat's turn stops it alone, at its next tick (`stop_task`).
    pub(super) fn claude_stop_subagent(&self, child: &Rt) {
        let (parent, call, done) = {
            let st = child.st.lock();
            (
                st.info.parent_id.clone(),
                st.info.parent_call.clone(),
                !matches!(
                    st.info.status,
                    SessionStatus::Running | SessionStatus::AwaitingApproval
                ),
            )
        };
        let (Some(parent), Some(call), false) = (parent, call, done) else {
            return;
        };
        let running = self
            .live_session(&parent)
            .is_some_and(|p| p.st.lock().running);
        if running {
            self.claude
                .stops
                .lock()
                .entry(parent)
                .or_default()
                .push(call);
        }
    }

    /// Ask the CLI for current context, throttled and request-correlated.
    fn claude_context_request(
        &self,
        t: &mut Turn,
        tx: &mpsc::UnboundedSender<String>,
        force: bool,
    ) {
        if !t.context_supported
            || (!force
                && t.context_last
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(2)))
        {
            return;
        }
        if t.context_request.is_some()
            && t.context_last
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        let id = format!("context-{}", new_id());
        t.context_request = Some(id.clone());
        t.context_last = Some(std::time::Instant::now());
        let _ = tx.send(cc::context_usage_line(&id));
    }

    /// Ask Claude Code to stop only the subagent started by `call`.
    fn claude_stop_subagent_now(
        &self,
        t: &mut Turn,
        call: &str,
        wtx: Option<&mpsc::UnboundedSender<String>>,
    ) {
        let Some(a) = t.agents.get_mut(call).filter(|a| !a.done && !a.stop_asked) else {
            return;
        };
        let task = t
            .tasks
            .iter()
            .find(|(_, (c, _))| c == call)
            .map(|(k, _)| k.clone());
        match (task, wtx) {
            (Some(task), Some(tx)) => {
                a.stop_asked = true;
                let _ = tx.send(cc::stop_task_line(&new_id(), &task));
            }
            // Its task isn't known yet (it is just starting): next time.
            (None, Some(_)) if !a.stop_asked => {
                self.claude
                    .stops
                    .lock()
                    .entry(t.sid.clone())
                    .or_default()
                    .push(call.to_string());
            }
            _ => {}
        }
    }

    /// A kept process of `sid` took a turn up by itself (its background work
    /// ended): it goes on as a turn of the chat, started as a message would
    /// start one, so its approvals, Tessera's tools, Stop and the completion
    /// check work as in any turn. Nothing that would ask is allowed by this.
    pub(super) fn claude_carried_on(self: &Arc<Self>, sid: &str) -> Handoff {
        let Some(rt) = self.live_session(sid) else {
            return Handoff::Refused("its chat isn't open");
        };
        let marks = {
            let st = rt.st.lock();
            // A helper works only through its runner (slot, budget, report,
            // worktree): what its kept process starts by itself is closed.
            if st.info.kind == SessionKind::Subagent {
                return Handoff::Refused("a helper's process doesn't start work by itself");
            }
            if st.running {
                return Handoff::Busy;
            }
            if st.rewinding {
                return Handoff::Refused("its chat is being rewound");
            }
            if !self.runs_claude_code(&st)
                || st.info.private
                || st.info.internal
                || is_room(&st.info)
            {
                return Handoff::Refused("its chat doesn't run on Claude Code now");
            }
            st.internal.turn_marks.clone()
        };
        // It goes on from its last turn: not across a Stop on this computer.
        if !self.running_here_since(&marks) {
            self.notice(
                &rt,
                NoticeLevel::Info,
                "Claude Code started working again by itself, but this computer was stopped since its last turn, so that was ended. Send a message to continue.",
            );
            return Handoff::Refused("this computer was stopped since its turn");
        }
        // Its Stop token is the chat's from the moment it is accepted: a Stop
        // right now stops this turn, not only a turn that is yet to begin.
        let cancel = CancellationToken::new();
        {
            let mut st = rt.st.lock();
            if st.running || st.rewinding {
                return Handoff::Busy;
            }
            st.running = true;
            st.cancel = Some(cancel.clone());
            st.next_turn_stops = Some(marks);
        }
        self.claude.resumed.lock().insert(sid.to_string());
        tracing::info!(
            "session {sid}: Claude Code took a turn up by itself (its background work ended); it goes on as a turn"
        );
        let me = self.clone();
        tokio::spawn(async move { me.run(rt, cancel).await });
        Handoff::Started
    }

    /// Keep a finished turn's process open for the chat's next turn, for
    /// `minutes` at most (see [`ClaudeProcs`]).
    fn claude_park(
        self: &Arc<Self>,
        sid: &str,
        proc: Proc,
        key: String,
        session: String,
        minutes: u32,
    ) {
        let cancel = CancellationToken::new();
        let id = new_id();
        let carried = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = {
            let (cancel, me, sid, id, carried) = (
                cancel.clone(),
                self.me.clone(),
                sid.to_string(),
                id.clone(),
                carried.clone(),
            );
            tokio::spawn(async move {
                let mut proc = proc;
                let idle = tokio::time::sleep(std::time::Duration::from_secs(minutes as u64 * 60));
                tokio::pin!(idle);
                let why = loop {
                    tokio::select! {
                        // Taken back or closed (a rewind) first, before any line.
                        biased;
                        _ = cancel.cancelled() => return Some(proc),
                        _ = &mut idle => break "idle",
                        line = proc.lines.next_line() => {
                            let Ok(Some(line)) = line else { break "it exited" };
                            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                            if parked_line(&v, proc.wtx.as_ref()) == ParkedLine::Quiet {
                                continue;
                            }
                            // It took a turn up by itself: the chat's next turn
                            // takes the process, starting with this line.
                            proc.backlog.push_back(line);
                            carried.store(true, std::sync::atomic::Ordering::SeqCst);
                            match hand_over(&me, &sid, &cancel).await {
                                None => return Some(proc),
                                Some(why) => break why,
                            }
                        }
                    }
                };
                tracing::debug!("session {sid}: closing its Claude Code process ({why})");
                if let Some(me) = me.upgrade() {
                    me.claude.forget(&sid, &id);
                }
                proc.close().await;
                None
            })
        };
        let parked = Parked {
            id,
            key,
            session,
            since: std::time::Instant::now(),
            cancel,
            task,
            carried,
        };
        let mut gone = Vec::new();
        {
            let mut all = self.claude.parked.lock();
            gone.extend(all.insert(sid.to_string(), parked));
            // A few at most: each holds a few hundred MB.
            while all.len() > MAX_PARKED {
                let Some(oldest) = all
                    .iter()
                    .min_by_key(|(_, p)| p.since)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                gone.extend(all.remove(&oldest));
            }
        }
        for p in gone {
            tokio::spawn(p.close());
        }
    }

    /// Persist only whether losing the opaque CLI roster changes completion
    /// reasons. In particular an idle background Bash has no helper child.
    fn claude_dependencies(&self, rt: &Rt, t: &Turn) {
        let changed = {
            let mut st = rt.st.lock();
            let outstanding = t.retain_background();
            std::mem::replace(&mut st.internal.claude_dependencies, outstanding) != outstanding
        };
        if changed {
            self.save(rt);
        }
    }
    /// Its input closes: approvals still open are dropped, and the bridge with
    /// them (whatever still runs stops, and nothing new runs).
    fn claude_close(&self, t: &mut Turn) {
        for (_, a) in t.approvals.drain() {
            a.cancel();
        }
        if let Some(b) = &t.bridge {
            b.cancel.cancel();
        }
    }

    /// Handle one message from Claude Code; `true` when its turn ended.
    fn claude_message(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        v: Value,
        wtx: Option<&mpsc::UnboundedSender<String>>,
        cancel: &CancellationToken,
    ) -> bool {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let sub = v.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind == "user"
            && v.get("isSynthetic").and_then(Value::as_bool) == Some(true)
            && v.pointer("/message/content/0/text")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("[Tessera work state, version "))
        {
            return false;
        }
        if kind != "stream_event" || sub {
            // Whatever this brings follows the pieces streamed so far.
            self.send_deltas(rt, t);
        }
        match kind {
            "system" => self.claude_system(rt, t, &v, wtx),
            "stream_event" if !sub => {
                self.claude_stream(rt, t, v.get("event").unwrap_or(&Value::Null))
            }
            // Its own error message (no sign-in, a failed request): the
            // turn's result says it, as a notice, not as a reply (kept in case
            // the result doesn't).
            "assistant" if v.get("error").is_some_and(|e| !e.is_null()) => {
                if !sub {
                    let msg = v.get("message").unwrap_or(&Value::Null);
                    let text: String =
                        cc::assistant_parts(msg.get("content").unwrap_or(&Value::Null))
                            .into_iter()
                            .filter_map(|p| match p {
                                AssistantPart::Text { text } => Some(text),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                    let error = v
                        .get("error")
                        .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_string));
                    t.api_error = Some(if text.trim().is_empty() {
                        format!("its request failed ({})", error.unwrap_or_default())
                    } else {
                        text.trim().to_string()
                    });
                }
            }
            "assistant" => {
                if !sub {
                    t.api_error = None;
                }
                let msg = v.get("message").unwrap_or(&Value::Null);
                let parts = cc::assistant_parts(msg.get("content").unwrap_or(&Value::Null));
                if sub {
                    // A subagent's steps go to its own session (shown like one of
                    // Tessera's subagents), and as progress on its card for
                    // clients from before that.
                    self.claude_flush(rt, t);
                    let parent = s(&v, "parent_tool_use_id").unwrap_or_default();
                    for p in &parts {
                        if let AssistantPart::ToolCall {
                            id,
                            name,
                            arguments,
                        } = p
                        {
                            t.note_check(id, name, arguments);
                            let line = format!(
                                "→ {} {}\n",
                                cc::shown_name(name),
                                one_line(&cc::summary(arguments, &t.cwd), 160)
                            );
                            self.claude_progress(rt, &parent, &line);
                            if let Some(a) = t.agents.get_mut(&parent) {
                                a.steps += 1;
                            }
                        }
                    }
                    self.claude_agent_message(t, &parent, msg, parts);
                    return false;
                }
                // Its subagents: each gets its session as soon as its call is there.
                for p in &parts {
                    if let AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } = p
                    {
                        t.note_check(id, name, arguments);
                        if cc::is_agent(name) {
                            self.claude_agent_start(rt, t, id, arguments);
                        }
                    }
                }
                self.claude_dependencies(rt, t);
                let id = s(msg, "id").unwrap_or_default();
                if t.pending.as_ref().is_some_and(|p| p.id != id) {
                    self.claude_flush(rt, t);
                }
                let usage = (!t.counted.contains(&id))
                    .then(|| cc::usage(msg.get("usage").unwrap_or(&Value::Null)));
                let p = t.pending.get_or_insert_with(|| Pending {
                    id: id.clone(),
                    ..Default::default()
                });
                p.parts.extend(parts.into_iter().map(shown_part));
                if let Some(u) = usage {
                    t.counted.push(id);
                    p.usage = u;
                }
            }
            // One of the messages sent to it, echoed as it takes it up.
            "user" if v.get("isReplay").and_then(Value::as_bool) == Some(true) => {
                if !sub && t.accept_input(&v) {
                    self.invalidate_completion(rt);
                }
            }
            "user" if !sub => {
                self.claude_flush(rt, t);
                let blocks = v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for b in blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                {
                    let Some(call_id) = s(b, "tool_use_id") else {
                        continue;
                    };
                    let Some(i) = t.open.iter().position(|(id, _)| *id == call_id) else {
                        continue;
                    };
                    let (_, name) = t.open.remove(i);
                    let failed = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    self.claude_tool_done(t, &call_id, &name, failed);
                    t.background_result(&call_id, &name, &v, failed);
                    // A bridged call: what Tessera's tool returned, with its
                    // details (a note's card, a board…), already redacted.
                    let bridged = t
                        .bridge
                        .as_ref()
                        .and_then(|br| br.done.lock().remove(&call_id));
                    let (content, is_error, details, artifact) = match bridged {
                        Some(out) => {
                            let artifact = out
                                .details
                                .as_ref()
                                .and_then(|d| d.get("artifact"))
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            (out.content, out.is_error, out.details, artifact)
                        }
                        None => {
                            let is_error =
                                b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                            let mut content =
                                cc::tool_result_parts(b.get("content").unwrap_or(&Value::Null));
                            self.redact_parts(&mut content);
                            // A subagent's call ends its session also when it failed or was
                            // stopped (its card still opens that session).
                            let details = if let Some(a) = t.agents.get_mut(&call_id) {
                                let details = a.details();
                                // One in the background goes on (its notification ends it).
                                if is_error || !a.background {
                                    let (status, why) = if a.stop_asked {
                                        (SessionStatus::Interrupted, Some(STOPPED_ALONE))
                                    } else if is_error {
                                        (SessionStatus::Error, None)
                                    } else {
                                        (SessionStatus::Idle, None)
                                    };
                                    let model =
                                        t.resolved.clone().unwrap_or_else(|| t.model.model.clone());
                                    if !a.done {
                                        self.invalidate_completion(rt);
                                        t.nudged = false;
                                    }
                                    self.claude_agent_end(
                                        a,
                                        &t.model.provider,
                                        &model,
                                        status,
                                        why,
                                    );
                                }
                                Some(details)
                            } else if is_error {
                                v.get("tool_use_result")
                                    .and_then(|r| cc::shell_details(&name, r))
                            } else if matches!(name.as_str(), "Edit" | "MultiEdit" | "Write") {
                                v.get("tool_use_result").cloned().and_then(|mut result| {
                                    self.redact_metadata(&mut result);
                                    cc::edit_details(&result, &t.cwd)
                                })
                            } else {
                                v.get("tool_use_result")
                                    .and_then(|r| cc::shell_details(&name, r))
                            };
                            (content, is_error, details, None)
                        }
                    };
                    let out = ToolOutput {
                        content: content.clone(),
                        is_error,
                        details: details.clone(),
                    };
                    if !is_error && let Some(args) = t.call_args.get(&call_id) {
                        self.claude_task_result(rt, &name, args, &v);
                    }
                    self.claude_observe(t, &call_id, &name, &out);
                    // Result text is not provenance: execution can return the
                    // same words as a CLI refusal. Only Tessera's exact-id
                    // recorded denial can refund a released claim.
                    let denied = t.denied.lock().get(&call_id).is_some_and(|o| !o.allowed());
                    self.finish_helper_step(rt, &call_id, denied);
                    let _ = self.store.tool_finished(&t.sid, &call_id);
                    let details = self.tool_timing(&t.sid, &call_id, details);
                    if let Some(st) = rt.st.lock().streaming.as_mut() {
                        st.running_tools.retain(|r| r.call_id != call_id);
                    }
                    if let Ok(e) = self.append(
                        rt,
                        EntryBody::ToolResult {
                            call_id,
                            tool: name,
                            content,
                            is_error,
                            artifact,
                            details,
                        },
                    ) {
                        t.record_last((e.seq, e.ts));
                    }
                    t.retire_agents();
                    self.claude_dependencies(rt, t);
                }
            }
            "user" => {
                let parent = s(&v, "parent_tool_use_id").unwrap_or_default();
                self.claude_agent_results(t, &parent, &v);
            }
            "control_request" => {
                let id = s(&v, "request_id").unwrap_or_default();
                let req = v.get("request").cloned().unwrap_or(Value::Null);
                match s(&req, "subtype").as_deref() {
                    Some("can_use_tool") => {
                        self.claude_flush(rt, t);
                        self.claude_permission(rt, t, id, req, wtx, cancel);
                    }
                    Some("mcp_message") => self.claude_mcp(rt, t, id, &req, wtx),
                    Some("hook_callback") => self.claude_hook(rt, t, id, &req, wtx),
                    _ => {
                        // Nothing else is served from here.
                        if let Some(tx) = wtx {
                            let _ = tx.send(cc::refusal_line(&id, "not supported by Tessera"));
                        }
                    }
                }
            }
            "control_cancel_request" => {
                if let Some(a) = s(&v, "request_id").and_then(|id| t.approvals.remove(&id)) {
                    a.cancel();
                }
            }
            "control_response" => {
                let reply = v.pointer("/response/response").unwrap_or(&Value::Null);
                let id = v.pointer("/response/request_id").and_then(Value::as_str);
                if id.is_some() && id == t.context_request.as_deref() {
                    t.context_request = None;
                    if v.pointer("/response/subtype").and_then(Value::as_str) == Some("success")
                        && let Some((tokens, window)) = cc::context_usage(reply)
                    {
                        let mut st = rt.st.lock();
                        st.info.context.tokens = tokens;
                        st.info.context.window = window;
                        st.info.context.source = Some("reported".into());
                        t.context_reported = true;
                        let _ = self.events.send(Event::SessionUpdated {
                            session: st.info.clone(),
                        });
                    } else {
                        t.context_supported = false;
                        let mut st = rt.st.lock();
                        st.info.context.source = Some("estimated".into());
                        let _ = self.events.send(Event::SessionUpdated {
                            session: st.info.clone(),
                        });
                    }
                }
                let models = cc::models_from_init(reply, &t.model.provider);
                if !models.is_empty()
                    && self
                        .providers
                        .set_discovered(&t.model.provider, models.clone())
                {
                    if let Ok(json) = serde_json::to_string(&models) {
                        let _ = self
                            .store
                            .kv_set(&claude_models_key(&t.model.provider), &json);
                    }
                    let _ = self.events.send(Event::ProvidersChanged);
                }
            }
            "rate_limit_event" => self.claude_rate_limit(rt, t, &v),
            "result" => {
                let failed = !cc::result_confirmed(&v);
                if failed || cancel.is_cancelled() {
                    self.claude_cut_off(rt, t, cancel.is_cancelled());
                }
                self.claude_flush(rt, t);
                self.end_stream(rt, t);
                t.finished = true;
                if t.claude_session.is_none() {
                    t.claude_session = s(&v, "session_id");
                }
                self.claude_usage(rt, t, &v);
                if failed && !cancel.is_cancelled() {
                    let unconfirmed = !s(&v, "subtype").is_some_and(|k| k.starts_with("error"))
                        && v.get("is_error").and_then(Value::as_bool) != Some(true);
                    let text = if unconfirmed {
                        "Claude Code ended with an unrecognized or incomplete result; this turn is not confirmed as finished. Send a message to try again.".into()
                    } else {
                        s(&v, "result")
                            .filter(|r| !r.trim().is_empty())
                            .or_else(|| {
                                v.get("errors").and_then(Value::as_array).map(|e| {
                                    e.iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join("; ")
                                })
                            })
                            .filter(|r| !r.trim().is_empty())
                            .unwrap_or_else(|| {
                                s(&v, "subtype").unwrap_or_else(|| "the turn failed".into())
                            })
                    };
                    t.failure = Some(text);
                } else {
                    // A limit only matters for the turn it stopped.
                    if t.api_error.is_none() {
                        t.limit = None;
                    }
                    // (Stopped: "Stopped." says it.)
                    let api_error = t.api_error.take().filter(|_| !cancel.is_cancelled());
                    if let Some(e) = api_error {
                        // Its request failed, though the result doesn't say so.
                        t.failure = Some(e);
                    } else if !cancel.is_cancelled()
                        && !t.said
                        && !t.compacted
                        && t.taken > t.answered
                    {
                        // An answer to the user's message with nothing in it: say so
                        // (or what it said only in its result, e.g. a command's output)
                        // rather than end in silence.
                        let text = s(&v, "result").filter(|r| !r.trim().is_empty());
                        let (level, text) = match text {
                            Some(r) => (
                                NoticeLevel::Info,
                                format!("Claude Code: {}", crate::util::truncate(r.trim(), 1500)),
                            ),
                            None => (
                                NoticeLevel::Warning,
                                "Claude Code ended its turn without a reply. Send a message to try again.".to_string(),
                            ),
                        };
                        self.notice(rt, level, text);
                    }
                }
                t.answered = t.taken;
                t.said = false;
                t.api_error = None;
                t.retry_noticed = false;
                t.carry_on_expected = false;
                return true;
            }
            _ => {}
        }
        false
    }

    /// The wait is visible without repeating the same count each tick.
    fn claude_wait_notice(&self, rt: &Rt, wait: &mut BackgroundWait, count: usize, retained: bool) {
        if wait.count_changed(count, retained) {
            self.notice(rt, NoticeLevel::Info, if count > 0 {
                format!("Waiting on Claude Code's background work ({count} running). Stop ends it; a new message goes straight in.")
            } else if retained {
                "Waiting on Claude Code's background work (0 confirmed running). A background request has no launch result yet. Stop ends it; a new message goes straight in.".into()
            } else {
                "Claude Code's background work ended. Waiting for its follow-up.".into()
            });
        }
    }

    /// `system` messages: the conversation started, it compacted itself, or
    /// one of its subagents (`Agent`) started, made progress or finished.
    fn claude_system(
        &self,
        rt: &Rt,
        t: &mut Turn,
        v: &Value,
        wtx: Option<&mpsc::UnboundedSender<String>>,
    ) {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        match s("subtype").as_deref() {
            Some("init") => {
                t.started = true;
                t.claude_session = s("session_id");
                t.resolved = s("model");
                // Fast mode was asked for but isn't on: say why, once.
                let state = s("fast_mode_state");
                if t.fast && !t.fast_noticed && state.as_deref() == Some("off") {
                    t.fast_noticed = true;
                    let why = s("fast_mode_disabled_reason").unwrap_or_default();
                    let text = cc::fast_off_reason(&why).map_or_else(
                        || "Claude Code didn't turn fast mode on for this chat; it runs at the usual speed.".to_string(),
                        |r| format!("{r} This chat runs at the usual speed."),
                    );
                    self.notice(rt, NoticeLevel::Info, text);
                }
                // Tessera's tools didn't connect this time: it goes on without them.
                let bridge = v
                    .get("mcp_servers")
                    .and_then(Value::as_array)
                    .and_then(|a| {
                        a.iter()
                            .find(|m| m.get("name").and_then(Value::as_str) == Some(cc::SERVER))
                    })
                    .and_then(|m| m.get("status"))
                    .and_then(Value::as_str);
                if t.bridge.is_some()
                    && matches!(bridge, Some("failed" | "needs-auth" | "disabled"))
                {
                    tracing::warn!(
                        "session {}: Tessera's tools didn't connect to Claude Code ({bridge:?})",
                        t.sid
                    );
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        "Tessera's own tools (notes, memory, whiteboards…) didn't connect to Claude Code this time; it carries on without them. Send your next message to try again.",
                    );
                }
                if let Some(src) = s("apiKeySource").filter(|k| k != "none")
                    && t.first
                {
                    self.notice(
                        rt,
                        NoticeLevel::Warning,
                        format!("Claude Code is using an API key here ({src}), so this chat is billed per token rather than to your Claude plan."),
                    );
                }
            }
            // Claude's API didn't answer and Claude Code tries again: said once a turn
            // (a long silence otherwise); if it gives up, its result says why.
            Some("api_retry") => {
                if !t.retry_noticed {
                    t.retry_noticed = true;
                    self.notice(rt, NoticeLevel::Warning, cc::api_retry(v));
                }
            }
            // It answers with another model than the chat's, or a model refused.
            Some(
                "model_fallback"
                | "model_refusal_fallback"
                | "model_consent_fallback"
                | "model_refusal_no_fallback",
            ) => {
                let text = s("content")
                    .filter(|c| !c.trim().is_empty())
                    .unwrap_or_else(|| match (s("original_model"), s("fallback_model")) {
                        (Some(a), Some(b)) => format!("switched from {a} to {b} for this turn."),
                        _ => "couldn't use this chat's model for this turn.".into(),
                    });
                self.notice(
                    rt,
                    NoticeLevel::Warning,
                    format!("Claude Code: {}", crate::util::truncate(text.trim(), 600)),
                );
            }
            Some("status") if s("status").as_deref() == Some("compacting") => {
                self.set_status(rt, SessionStatus::Compacting);
            }
            Some("compact_boundary") => {
                let before = n("/compact_metadata/pre_tokens");
                let after = n("/compact_metadata/post_tokens");
                {
                    let mut st = rt.st.lock();
                    if after > 0 {
                        st.info.context.tokens = after;
                    }
                    st.info.context.source = Some("estimated".into());
                    st.info.context.compactions += 1;
                    let _ = self.events.send(Event::SessionUpdated {
                        session: st.info.clone(),
                    });
                }
                let _ = self.append(
                    rt,
                    EntryBody::Compaction {
                        external: true,
                        summary: String::new(),
                        first_kept_seq: 0, // CLI's hidden conversation is opaque.
                        tokens_before: before,
                        tokens_after: after,
                    },
                );
                self.set_status(rt, SessionStatus::Running);
                let (_, update) = ledger_context(rt.st.lock().info.task.as_ref());
                if let Some(tx) = wtx {
                    let _ = tx.send(cc::context_line(&update));
                    self.claude_context_request(t, tx, true);
                }
                t.compacted = true;
            }
            Some(
                kind @ ("task_started" | "task_progress" | "task_notification" | "task_updated"),
            ) => {
                self.claude_flush(rt, t);
                let task = s("task_id");
                // Preserve the canonical association learned at start OR result.
                let call = task
                    .as_ref()
                    .and_then(|id| t.tasks.get(id))
                    .map(|(c, _)| c.clone())
                    .filter(|c| !c.is_empty())
                    .or_else(|| s("tool_use_id"));
                let moved = kind == "task_updated"
                    && v.pointer("/patch/is_backgrounded").and_then(Value::as_bool) == Some(true);
                let terminal = kind == "task_notification"
                    || t.background_ended(task.as_deref(), call.as_deref());
                if terminal {
                    if t.finish_background(task.as_deref(), call.as_deref()) {
                        self.invalidate_completion(rt);
                        t.nudged = false;
                    }
                } else if let Some(task) = task.as_ref() {
                    let known = call.as_ref().is_some_and(|call| {
                        t.tasks.values().any(|(c, bg)| c == call && *bg)
                            || t.agents
                                .get(call)
                                .is_some_and(|a| a.background || a.requested_background)
                            || t.call_args
                                .get(call)
                                .and_then(|args| args.get("run_in_background"))
                                .and_then(Value::as_bool)
                                == Some(true)
                    });
                    if kind == "task_started" || moved {
                        let background = moved
                            || known
                            || v.get("is_backgrounded")
                                .and_then(Value::as_bool)
                                .unwrap_or_else(|| s("task_type").as_deref() == Some("local_bash"));
                        t.link_background(
                            task.clone(),
                            call.clone().unwrap_or_default(),
                            background,
                        );
                    }
                }
                self.claude_dependencies(rt, t);
                let Some(call) = call else {
                    return;
                };
                let open = t
                    .open
                    .iter()
                    .find(|(id, _)| *id == call)
                    .map(|(_, name)| name.clone());
                // Claude Code reports a command it runs in the background as a
                // task too: that is no subagent, just a note on its card.
                if !t.agents.contains_key(&call) {
                    let background = t.tasks.values().any(|(c, bg)| *c == call && *bg);
                    if open.is_some() && kind == "task_started" && background {
                        self.claude_progress(rt, &call, "Runs in the background.\n");
                    } else if open.is_some() && moved {
                        self.claude_progress(
                            rt,
                            &call,
                            "Moved to the background after its time limit; it goes on.\n",
                        );
                    }
                    t.retire_agents();
                    return;
                }
                let background = t.tasks.values().any(|(c, bg)| *c == call && *bg);
                if background && let Some(a) = t.agents.get_mut(&call) {
                    a.background = true;
                    if moved {
                        self.claude_progress(rt, &call, "Moved to the background; it goes on.\n");
                    }
                }
                // A background subagent's last word comes after its call's result.
                if terminal {
                    let model = t.resolved.clone().unwrap_or_else(|| t.model.model.clone());
                    let a = t.agents.get_mut(&call).expect("checked above");
                    a.status = s("status")
                        .or_else(|| {
                            v.pointer("/patch/status")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    let status = if a.stop_asked {
                        SessionStatus::Interrupted
                    } else {
                        match a.status.as_str() {
                            "completed" => SessionStatus::Idle,
                            "failed" => SessionStatus::Error,
                            _ => SessionStatus::Interrupted,
                        }
                    };
                    let why = a.stop_asked.then_some(STOPPED_ALONE);
                    self.claude_agent_end(a, &t.model.provider, &model, status, why);
                    t.retire_agents();
                    return;
                }
                if open.is_none() || kind == "task_updated" {
                    return;
                }
                let a = t.agents.entry(call.clone()).or_default();
                if let Some(d) = s("description") {
                    a.description = d;
                }
                if let Some(k) = s("subagent_type") {
                    if let Some(c) = &a.child {
                        c.st.lock().info.role = Some(k.clone());
                    }
                    a.kind = k;
                }
                if v.get("is_backgrounded").and_then(Value::as_bool) == Some(true) {
                    a.background = true;
                }
                if v.get("usage").is_some() {
                    a.tool_uses = n("/usage/tool_uses");
                    a.tokens = n("/usage/total_tokens");
                    a.duration_ms = n("/usage/duration_ms");
                }
                let line = match kind {
                    "task_started" => Some(format!(
                        "{} started: {}\n",
                        if a.kind.is_empty() {
                            "Subagent"
                        } else {
                            a.kind.as_str()
                        },
                        one_line(&a.description, 120)
                    )),
                    "task_progress" => s("summary").map(|x| format!("· {}\n", one_line(&x, 160))),
                    _ => {
                        a.status = s("status").unwrap_or_default();
                        None
                    }
                };
                if let Some(line) = line {
                    self.claude_progress(rt, &call, &line);
                }
            }
            _ => {}
        }
    }

    /// Public plan facts, plus one near-limit heads-up. A rejected turn's
    /// notice belongs to the retry wrapper, so it says exactly what happens.
    fn claude_rate_limit(&self, rt: &Rt, t: &mut Turn, v: &Value) {
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return;
        }
        let info = v.get("rate_limit_info").unwrap_or(&Value::Null);
        self.providers.plan_limits().record(
            &t.model.provider,
            crate::limits::from_claude(info, now_ms()),
            false,
        );
        if let Some(hit) = limits::Hit::event(info) {
            t.limit = Some(hit);
        } else if !t.limit_noticed
            && let Some(text) = limits::warning(info)
        {
            t.limit_noticed = true;
            self.notice(rt, NoticeLevel::Info, text);
        }
    }

    /// An MCP message for the bridge. The handshake and the tool list are
    /// answered at once; a tool call runs through Tessera's own dispatch for
    /// this chat, in the background (it may wait for an approval), and its
    /// result goes back when done.
    fn claude_mcp(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        request_id: String,
        req: &Value,
        wtx: Option<&mpsc::UnboundedSender<String>>,
    ) {
        self.check_budget(rt);
        let Some(tx) = wtx.cloned() else { return };
        let server = req
            .get("server_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(bridge) = t.bridge.as_mut().filter(|_| server == cc::SERVER) else {
            let _ = tx.send(cc::refusal_line(
                &request_id,
                &format!("Tessera has no MCP server `{server}` for this chat"),
            ));
            return;
        };
        let msg = req.get("message").cloned().unwrap_or(Value::Null);
        // A rare settings change trades cache stability for an honest list.
        bridge.tools = self.claude_bridge_tools(&rt.st.lock());
        let (id, name, arguments, call) = match cc::mcp_request(&msg, &bridge.tools) {
            cc::Rpc::Reply(v) => {
                let _ = tx.send(cc::mcp_line(&request_id, v));
                return;
            }
            cc::Rpc::Call {
                id,
                name,
                arguments,
                call,
            } => (id, name, arguments, call),
        };
        let identified_call = call;
        let call_id = identified_call.clone().unwrap_or_else(new_id);
        let mut provisional = super::helper_limits::Provisional::new(self, rt, &call_id);
        let reply = move |content: &[ContentPart], is_error: bool| {
            cc::mcp_line(&request_id, cc::mcp_result(&id, content, is_error))
        };
        // Offered tools only, and only while the turn is on.
        let offered: Vec<&str> = bridge.tools.iter().map(|s| s.name.as_str()).collect();
        if name == "save_secret"
            && arguments.as_object().is_none_or(|a| {
                a.keys()
                    .any(|k| !["action", "name", "title", "kind", "url"].contains(&k.as_str()))
            })
        {
            let _ = tx.send(reply(&[ContentPart::text("Use the secure entry card. Credential values are not accepted as Claude tool arguments.")], true));
            return;
        }
        let tool = offered
            .contains(&name.as_str())
            .then(|| self.claude_capabilities(&rt.st.lock()).contains(&name))
            .filter(|ok| *ok)
            .and_then(|_| self.registry.read().get(&name));
        let refusal = if bridge.cancel.is_cancelled() {
            Some("This turn is over; Tessera's tools are no longer available to it.".to_string())
        } else if tool.is_none() {
            let st = rt.st.lock();
            Some(if st.info.kind == SessionKind::Subagent {
                self.unavailable_tool(&st, &name, &self.claude_capabilities(&st))
            } else {
                format!(
                    "Unknown tool `{name}`. Tessera's tools here: {}.",
                    offered.join(", ")
                )
            })
        } else {
            None
        };
        let (done, cancel) = (bridge.done.clone(), bridge.cancel.clone());
        let (Some(tool), None) = (tool, refusal.clone()) else {
            let why = refusal.unwrap_or_default();
            let _ = tx.send(reply(&[ContentPart::text(why)], true));
            return;
        };
        // Its card is in the transcript before it runs (and asks).
        self.claude_flush(rt, t);
        // Transcript cards are presentation, never authorization.
        let main = t.open.iter().any(|(c, _)| *c == call_id);
        if name == "schedule"
            && let Err(why) = crate::tools::meta::schedule_action(&arguments)
        {
            let _ = tx.send(reply(&[ContentPart::text(why)], true));
            return;
        }
        if delegation_tool(&name, &arguments) {
            let mut admissions = t.admissions.lock();
            let retired = admissions.retired(&call_id);
            if retired {
                tracing::warn!(call_id = %call_id, "claude code: refused delegation replay");
            }
            let admitted = admissions.admitted.remove(&call_id);
            // Removing an admission consumes it even if dispatch validation
            // fails. The same id must not recover authority through a re-hook.
            if admitted.is_some() {
                admissions.consumed.insert(call_id.clone());
            }
            let valid = identified_call.is_some()
                && !retired
                && msg.pointer("/params/arguments") == Some(&arguments)
                && main
                && !t.denied.lock().contains_key(&call_id)
                && admitted.is_some_and(|a| {
                    a.tool == format!("mcp__tessera__{name}")
                        && a.input == arguments
                        && a.mode == self.effective_permission(&rt.st.lock())
                        && t.policy_hooks.get(&call_id).is_some_and(|actor| {
                            actor == &a.actor
                                && main_actor(actor)
                                && actor.get("tool_use_id").and_then(Value::as_str)
                                    == Some(call_id.as_str())
                                && actor.get("tool_name").and_then(Value::as_str)
                                    == Some(a.tool.as_str())
                                && actor.get("input") == Some(&arguments)
                        })
                });
            if !valid {
                let _ = tx.send(reply(&[ContentPart::text("Delegation requires an admitted main-chat call with unchanged id, tool, input and actor.")], true));
                return;
            }
            // The consumed tombstone survives failure/cancellation/completion.
            admissions.in_flight.insert(call_id.clone());
        }
        // Only CLI hook metadata may supply actor authority. Unknown child
        // provenance is read-only; model tool arguments cannot widen it.
        let actor_read_only = t
            .policy_hooks
            .get(&call_id)
            .map(|actor| {
                actor_is_read_only(actor)
                    || actor
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .and_then(cc::native_name)
                        != Some(name.as_str())
                    || actor.get("input") != Some(&arguments)
            })
            .unwrap_or(!main);
        // The screen is the main chat's: Claude Code's helpers share its
        // session (the screen can't tell them apart) and can't be stopped one
        // by one.
        if name == "desktop" && !main {
            let _ = tx.send(reply(
                &[ContentPart::text(
                    "Helpers cannot use a computer's screen; the main chat can.",
                )],
                true,
            ));
            return;
        }
        let seq = t
            .calls
            .get(&call_id)
            .copied()
            .unwrap_or_else(|| rt.st.lock().info.last_seq);
        provisional.transfer();
        let me = self.clone();
        let rt = rt.clone();
        let admissions = t.admissions.clone();
        tokio::spawn(async move {
            // Claude Code waits for an answer to every call: one goes back even
            // if the tool failed outright.
            let run = {
                let call_id = call_id.clone();
                tokio::spawn(async move {
                    me.execute_tool_ext(
                        &rt,
                        tool,
                        &call_id,
                        arguments,
                        seq,
                        &cancel,
                        false,
                        actor_read_only,
                    )
                    .await
                })
            };
            let out = match run.await {
                Ok(out) => out,
                Err(e) => {
                    tracing::error!("claude code: Tessera's tool `{name}` failed: {e}");
                    ToolOutput::error(format!(
                        "Tessera's tool `{name}` failed unexpectedly; try again or carry on without it."
                    ))
                }
            };
            let line = reply(&out.content, out.is_error);
            admissions.lock().in_flight.remove(&call_id);
            done.lock().insert(call_id, out);
            let _ = tx.send(line);
        });
    }

    /// Live text, reasoning and tool-call arguments as they stream.
    fn claude_stream(&self, rt: &Rt, t: &mut Turn, ev: &Value) {
        let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let llm = match ev.get("type").and_then(Value::as_str).unwrap_or_default() {
            "message_start" => {
                let id = ev
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if t.pending.as_ref().is_some_and(|p| p.id != id) {
                    self.claude_flush(rt, t);
                }
                t.current = Some(id.to_string());
                t.tool_blocks.clear();
                self.start_stream(rt, t);
                return;
            }
            "content_block_start" => {
                let b = ev.get("content_block").unwrap_or(&Value::Null);
                if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                    return;
                }
                let id = b
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name =
                    cc::shown_name(b.get("name").and_then(Value::as_str).unwrap_or_default())
                        .to_string();
                t.tool_blocks.insert(index, (id.clone(), name.clone()));
                LlmEvent::ToolCall {
                    index,
                    id,
                    name,
                    arguments: String::new(),
                }
            }
            "content_block_delta" => {
                let d = ev.get("delta").unwrap_or(&Value::Null);
                let text = |k: &str| {
                    d.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                match d.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "text_delta" => LlmEvent::Text {
                        index,
                        text: text("text"),
                    },
                    "thinking_delta" if !text("thinking").is_empty() => LlmEvent::Thinking {
                        index,
                        text: text("thinking"),
                    },
                    "input_json_delta" => {
                        let Some((id, name)) = t.tool_blocks.get(&index).cloned() else {
                            return;
                        };
                        LlmEvent::ToolCall {
                            index,
                            id,
                            arguments: if name == "save_secret" {
                                String::new()
                            } else {
                                text("partial_json")
                            },
                            name,
                        }
                    }
                    _ => return,
                }
            }
            "message_delta" => {
                let u = cc::usage(ev.get("usage").unwrap_or(&Value::Null));
                if let Some(p) = t
                    .pending
                    .as_mut()
                    .filter(|p| Some(&p.id) == t.current.as_ref())
                    && u.output > 0
                {
                    p.usage.output = u.output;
                }
                let mut st = rt.st.lock();
                let context = u.input + u.cache_read + u.cache_write + u.output;
                if context > 0 && !t.context_reported {
                    st.info.context.tokens = context;
                    st.info.context.source = Some("estimated".into());
                }
                return;
            }
            _ => return,
        };
        if !t.streaming {
            self.start_stream(rt, t);
        }
        // Batched like Tessera's own replies (see `deltas.rs`).
        for ev in t.deltas.push(llm) {
            super::deltas::send(rt, &self.events, &t.sid, ev);
        }
    }

    /// Streamed pieces still waiting go out now (before anything after them).
    fn send_deltas(&self, rt: &Rt, t: &mut Turn) {
        if let Some(ev) = t.deltas.take() {
            super::deltas::send(rt, &self.events, &t.sid, ev);
        }
    }

    fn start_stream(&self, rt: &Rt, t: &mut Turn) {
        self.send_deltas(rt, t);
        {
            let mut st = rt.st.lock();
            let running = st
                .streaming
                .take()
                .map(|s| s.running_tools)
                .unwrap_or_default();
            st.streaming = Some(StreamingMessage {
                content: vec![],
                model: t.model.clone(),
                author: t.author.clone(),
                running_tools: running,
            });
        }
        t.streaming = true;
        let _ = self.events.send(Event::StreamStarted {
            session_id: t.sid.clone(),
            model: t.model.clone(),
            author: t.author.clone(),
        });
    }

    /// End the live message, keeping the tools still running.
    fn end_stream(&self, rt: &Rt, t: &mut Turn) {
        self.send_deltas(rt, t);
        if !std::mem::take(&mut t.streaming) {
            return;
        }
        super::deltas::finish(rt, &self.events, &t.sid);
        {
            let mut st = rt.st.lock();
            if let Some(s) = st.streaming.as_mut() {
                s.content.clear();
            }
        }
        let _ = self.events.send(Event::StreamEnded {
            session_id: t.sid.clone(),
        });
    }

    /// Text that streamed but never arrived as a finished block (stopped or
    /// ended mid-sentence) stays in the transcript instead of vanishing,
    /// after the message's finished blocks.
    fn claude_cut_off(&self, rt: &Rt, t: &mut Turn, stopped: bool) {
        self.send_deltas(rt, t);
        let streamed: Vec<String> = rt
            .st
            .lock()
            .streaming
            .as_ref()
            .map(|s| {
                s.content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } if !text.trim().is_empty() => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let finished: Vec<String> = t
            .pending
            .iter()
            .flat_map(|p| &p.parts)
            .filter_map(|p| match p {
                AssistantPart::Text { text } => Some(text.trim().to_string()),
                _ => None,
            })
            .collect();
        let cut: Vec<AssistantPart> = streamed
            .into_iter()
            .filter(|s| !finished.iter().any(|f| f.starts_with(s.trim())))
            .map(|text| AssistantPart::Text { text })
            .collect();
        if cut.is_empty() {
            return;
        }
        self.claude_flush(rt, t);
        let _ = self.append(
            rt,
            EntryBody::Assistant {
                content: cut,
                model: t.model.clone(),
                usage: Usage::default(),
                stop: StopReason::Aborted,
                error: (!stopped).then(|| "Cut off: Claude Code ended mid-reply.".to_string()),
                author: t.author.clone(),
            },
        );
    }

    /// The assistant message so far becomes a transcript entry; its tool
    /// calls are running from here on.
    fn claude_flush(&self, rt: &Rt, t: &mut Turn) {
        let Some(p) = t.pending.take() else { return };
        self.end_stream(rt, t);
        if p.parts.is_empty() {
            return;
        }
        let context = p.usage.input + p.usage.cache_read + p.usage.cache_write + p.usage.output;
        if context > 0 && !t.context_reported {
            let mut st = rt.st.lock();
            st.info.context.tokens = context;
            st.info.context.source = Some("estimated".into());
        }
        let calls: Vec<(String, String)> = p
            .parts
            .iter()
            .filter_map(|p| match p {
                AssistantPart::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
                _ => None,
            })
            .collect();
        let stop = if calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        };
        match self.append(
            rt,
            EntryBody::Assistant {
                content: p.parts,
                model: t.model.clone(),
                usage: p.usage,
                stop,
                error: None,
                author: t.author.clone(),
            },
        ) {
            Ok(e) => {
                t.record_last((e.seq, e.ts));
                t.said = true;
                for (id, _) in &calls {
                    t.calls.insert(id.clone(), e.seq);
                }
            }
            Err(e) => tracing::error!("claude code: saving a reply: {e}"),
        }
        for (id, name) in calls {
            let _ = self.store.tool_started(&t.sid, &id, &name);
            {
                let mut st = rt.st.lock();
                if st.budget.is_none() {
                    st.info.steps += 1;
                }
                let model = st.info.model.clone();
                let s = st.streaming.get_or_insert_with(|| StreamingMessage {
                    content: vec![],
                    model,
                    author: None,
                    running_tools: vec![],
                });
                s.running_tools.push(RunningTool {
                    call_id: id.clone(),
                    name: name.clone(),
                    output: String::new(),
                });
            }
            let _ = self.events.send(Event::ToolStarted {
                session_id: t.sid.clone(),
                call_id: id.clone(),
                name: name.clone(),
            });
            t.open.push((id, name));
        }
    }

    fn claude_progress(&self, rt: &Rt, call_id: &str, chunk: &str) {
        let chunk = crate::tools::redact_known(chunk, &self.redaction_secrets());
        let chunk = chunk.as_str();
        let (sid, known) = {
            let mut st = rt.st.lock();
            let sid = st.info.id.clone();
            let t = st
                .streaming
                .as_mut()
                .and_then(|s| s.running_tools.iter_mut().find(|t| t.call_id == call_id));
            let known = t.is_some();
            if let Some(t) = t {
                t.output.push_str(chunk);
            }
            (sid, known)
        };
        if known {
            let _ = self.events.send(Event::ToolOutput {
                session_id: sid,
                call_id: call_id.to_string(),
                chunk: chunk.to_string(),
            });
        }
    }

    pub(super) fn redact_parts(&self, parts: &mut [ContentPart]) {
        let secrets = self.redaction_secrets();
        let patterns = self.config.read().security.redact_patterns;
        for p in parts {
            if let ContentPart::Text { text } = p {
                *text = if patterns {
                    crate::tools::redact(text, &secrets)
                } else {
                    crate::tools::redact_known(text, &secrets)
                };
            }
        }
    }

    /// Answer a permission prompt: by the chat's permission mode and
    /// Tessera's own command policy where that decides, else by asking the
    /// user (the same approval as for Tessera's tools).
    fn claude_permission(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        request_id: String,
        req: Value,
        wtx: Option<&mpsc::UnboundedSender<String>>,
        cancel: &CancellationToken,
    ) {
        let Some(tx) = wtx.cloned() else { return };
        let tool = req
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let input = req.get("input").cloned().unwrap_or_else(|| json!({}));
        if tool == "AskUserQuestion"
            && matches!(
                self.claude_policy(rt, &t.cwd, &tool, &input, &req),
                Policy::Allow
            )
        {
            return self.claude_ask_user(rt, t, request_id, req, tx, cancel);
        }
        self.claude_gate(rt, t, request_id, req, tx, false);
    }

    /// PreToolUse and can_use_tool share one card, keyed by the real call id
    /// and exact input. An empty successful hook does not override CLI safety.
    fn claude_gate(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        request_id: String,
        req: Value,
        tx: mpsc::UnboundedSender<String>,
        hook: bool,
    ) {
        self.check_budget(rt);
        let mut req = req;
        let mut provenance_conflict = false;
        if let Some(call) = req
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if hook {
                t.policy_hooks
                    .entry(call.clone())
                    .or_insert_with(|| req.clone());
            }
            if let Some(original) = t.policy_hooks.get(&call)
                && original.get("tool_name") == req.get("tool_name")
                && original.get("input") == req.get("input")
            {
                // Explicit metadata (including null or malformed values) is
                // evidence, not something to overwrite with main authority.
                // Only genuinely omitted can_use_tool fields inherit.
                let keys = [
                    "agent_id",
                    "agent_type",
                    "permission_mode",
                    "cwd",
                    "mcp_server",
                ];
                provenance_conflict = keys.iter().any(|key| {
                    req.get(*key)
                        .is_some_and(|value| original.get(*key) != Some(value))
                });
                if !provenance_conflict && !hook {
                    for key in keys {
                        if req.get(key).is_none()
                            && let Some(value) = original.get(key)
                        {
                            req[key] = value.clone();
                        }
                    }
                }
            }
        }
        let input = req.get("input").cloned().unwrap_or(Value::Null);
        let tool = req
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let call = req
            .get("tool_use_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string);
        let mode = self.effective_permission(&rt.st.lock());
        if let Some(call) = &call {
            t.admissions.lock().admitted.remove(call);
            let mut observed = input.clone();
            if let Some(cwd) = req.get("cwd").and_then(Value::as_str) {
                observed["cwd"] = json!(cwd);
                if let Some(path) = cc::edit_path(&input) {
                    observed["path"] = json!(Path::new(cwd).join(path));
                }
            }
            t.call_args.insert(call.clone(), observed);
        }
        // Plan the bookkeeping now; no file is read or stored before `answer`
        // is called with an affirmative policy/approval decision.
        let snapshot = if hook && t.snapshots && cc::is_edit_tool(&tool) {
            call.as_ref()
                .zip(cc::edit_path(&input))
                .map(|(call, path)| {
                    let path = req
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(Path::new)
                        .unwrap_or(&t.cwd)
                        .join(path);
                    self.claude_flush(rt, t);
                    let seq = self.claude_call_seq(rt, t, call);
                    t.edits.insert(call.clone(), (path.clone(), seq));
                    if !t.snapped.contains(&seq) {
                        t.snapped.push(seq);
                    }
                    (t.sid.clone(), seq, path)
                })
        } else {
            None
        };
        let approval_id = request_id.clone();
        // A helper reports to the chat that started it; it has no chat to ask in.
        let helper = super::helper_approvals::is_helper(&rt.st.lock());
        // Allowed only if no Stop on this computer came since this turn
        // began, up to the moment the answer goes to Claude Code.
        let stops_at = Arc::new(rt.st.lock().turn_stops.clone());
        let answer_stops = stops_at.clone();
        let (me, reply_rt, reply_cancel, reply_cwd, reply_tool, reply_req) = (
            self.clone(),
            rt.clone(),
            t.cancel.clone(),
            t.cwd.clone(),
            tool.clone(),
            req.clone(),
        );
        let admissions = t.admissions.clone();
        let denied_calls = t.denied.clone();
        let admission_call = call.clone();
        // Only the mandatory hook can validate provenance, not a transcript
        // card or can_use_tool's model arguments.
        let admission = call
            .as_ref()
            .and_then(|id| t.policy_hooks.get(id))
            .filter(|actor| {
                main_actor(actor)
                    && [
                        "agent_id",
                        "agent_type",
                        "permission_mode",
                        "cwd",
                        "mcp_server",
                    ]
                    .iter()
                    .all(|key| actor.get(*key) == req.get(*key))
                    && actor.get("tool_use_id").and_then(Value::as_str) == call.as_deref()
                    && actor.get("tool_name") == req.get("tool_name")
                    && actor.get("input") == req.get("input")
                    && input.is_object()
            })
            .map(|actor| MainAdmission {
                tool: tool.clone(),
                input: input.clone(),
                actor: actor.clone(),
                mode,
            });
        let answer = move |outcome: ApprovalOutcome| {
            let call_id = reply_req
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut provisional = super::helper_limits::Provisional::new(&me, &reply_rt, &call_id);
            if matches!(outcome, ApprovalOutcome::Rule(_) | ApprovalOutcome::Stopped) {
                approval::audit(
                    &reply_rt.st.lock().info.id,
                    &reply_tool,
                    &Answer::rule(outcome.clone()),
                );
            }
            let outcome = if outcome.allowed() {
                let name = cc::native_name(&reply_tool).unwrap_or(&reply_tool);
                match me.reserve_helper_step(
                    &reply_rt,
                    name,
                    &input,
                    reply_req.get("tool_use_id").and_then(Value::as_str),
                    false,
                ) {
                    Ok(()) => outcome,
                    Err(why) => ApprovalOutcome::Rule(why.into()),
                }
            } else {
                outcome
            };
            let allow = outcome.allowed();
            if !allow && let Some(id) = &admission_call {
                denied_calls.lock().insert(id.clone(), outcome.clone());
            }
            let why = outcome.message_in(&reply_tool, helper);
            let line = if hook {
                cc::hook_line(
                    &request_id,
                    if allow {
                        json!({})
                    } else {
                        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": why}})
                    },
                )
            } else {
                cc::permission_line(&request_id, allow, &input, &why)
            };
            if allow && let Some((sid, seq, path)) = snapshot {
                tokio::spawn(async move {
                    let permitted = || {
                        !reply_cancel.is_cancelled()
                            && me.running_here_since(&answer_stops)
                            && me.effective_permission(&reply_rt.st.lock()) == mode
                            && !matches!(
                                me.claude_policy(
                                    &reply_rt,
                                    &reply_cwd,
                                    &reply_tool,
                                    &input,
                                    &reply_req
                                ),
                                Policy::Deny(_)
                            )
                    };
                    let before_copy = permitted();
                    let mut copy_error = None;
                    if before_copy {
                        let (depth, admission) = {
                            let st = reply_rt.st.lock();
                            (st.depth, me.action_admission(&st))
                        };
                        let ctx = ToolCtx {
                            session_id: sid,
                            call_id: String::new(),
                            seq,
                            cwd: reply_req
                                .get("cwd")
                                .and_then(Value::as_str)
                                .map(PathBuf::from)
                                .unwrap_or_else(|| reply_cwd.clone()),
                            cancel: reply_cancel.clone(),
                            host: Arc::new(Host(me.clone(), actor_is_read_only(&reply_req))),
                            progress: Arc::new(|_| {}),
                            depth,
                            cleared: None,
                            admission,
                        };
                        copy_error = match tokio::task::spawn_blocking(move || {
                            snapshot_before_edit(&ctx, &path)
                        })
                        .await
                        {
                            Ok(result) => result.err(),
                            Err(e) => {
                                tracing::error!("claude code: a copy before an edit failed: {e}");
                                Some("The edit target could not be safely snapshotted.".into())
                            }
                        };
                    }
                    // Permission may have changed while the copy was being taken.
                    let accepted = before_copy && copy_error.is_none() && permitted();
                    let line = if accepted {
                        line
                    } else {
                        let why = copy_error.as_deref().unwrap_or_else(|| {
                            if me.running_here_since(&answer_stops) {
                                "Permissions changed; the edit was not performed."
                            } else {
                                STOPPED_SINCE
                            }
                        });
                        cc::hook_line(
                            &request_id,
                            json!({"hookSpecificOutput": {
                                "hookEventName": "PreToolUse", "permissionDecision": "deny",
                                "permissionDecisionReason": why
                            }}),
                        )
                    };
                    if accepted {
                        if let Err(why) = provisional.release_to(|| tx.send(line).is_ok()) {
                            let _ = tx.send(cc::hook_line(&request_id, json!({"hookSpecificOutput": {
                                "hookEventName":"PreToolUse", "permissionDecision":"deny", "permissionDecisionReason":why
                            }})));
                        }
                    } else {
                        let _ = tx.send(line);
                    }
                });
            } else if allow && !me.running_here_since(&answer_stops) {
                if let Some(id) = &admission_call {
                    denied_calls
                        .lock()
                        .insert(id.clone(), ApprovalOutcome::Stopped);
                }
                let _ = tx.send(if hook {
                    cc::hook_line(
                        &request_id,
                        json!({"hookSpecificOutput": {
                            "hookEventName": "PreToolUse", "permissionDecision": "deny",
                            "permissionDecisionReason": STOPPED_SINCE
                        }}),
                    )
                } else {
                    cc::permission_line(&request_id, false, &input, STOPPED_SINCE)
                });
            } else {
                if allow
                    && (reply_cancel.is_cancelled()
                        || me.effective_permission(&reply_rt.st.lock()) != mode)
                {
                    let why = "Cancelled or permissions changed; not executed.";
                    let _ = tx.send(if hook { cc::hook_line(&request_id, json!({"hookSpecificOutput": {"hookEventName":"PreToolUse", "permissionDecision":"deny", "permissionDecisionReason":why}})) } else { cc::permission_line(&request_id, false, &input, why) });
                    return;
                }
                // Check retirement and enqueue/publish under the dispatch lock:
                // a late gate answer must never recreate consumed authority.
                let mut admissions = admissions.lock();
                let enqueue = |available: bool| {
                    let why = if allow && !available {
                        "This delegation call was already dispatched; use a new call id."
                    } else if allow {
                        match provisional.release_to(|| tx.send(line).is_ok()) {
                            Ok(queued) => return queued,
                            Err(why) => why,
                        }
                    } else {
                        return tx.send(line).is_ok();
                    };
                    if let Some(id) = &admission_call {
                        denied_calls
                            .lock()
                            .insert(id.clone(), ApprovalOutcome::Rule(why.into()));
                    }
                    let _ = tx.send(if hook {
                        cc::hook_line(
                            &request_id,
                            json!({"hookSpecificOutput": {
                                "hookEventName": "PreToolUse", "permissionDecision": "deny",
                                "permissionDecisionReason": why
                            }}),
                        )
                    } else {
                        cc::permission_line(&request_id, false, &input, why)
                    });
                    false
                };
                if let Some(id) = admission_call.clone() {
                    admissions.publish(id, allow.then_some(admission).flatten(), enqueue);
                } else {
                    enqueue(true);
                }
            }
        };
        let input = req.get("input").cloned().unwrap_or(Value::Null);
        let Some(call_id) = call else {
            return answer(ApprovalOutcome::Rule(
                "Claude Code did not identify this action; submit a valid tool call.".into(),
            ));
        };
        if t.admissions.lock().retired(&call_id) {
            tracing::warn!(call_id = %call_id, "claude code: refused delegation admission rearm");
            return answer(ApprovalOutcome::Rule(
                "This delegation call was already dispatched; use a new call id.".into(),
            ));
        }
        if provenance_conflict {
            return answer(ApprovalOutcome::Rule(
                "The action's actor metadata conflicts with its mandatory hook; submit a new call."
                    .into(),
            ));
        }
        if t.cancel.is_cancelled() {
            return answer(ApprovalOutcome::Dismissed);
        }
        if !input.is_object() {
            return answer(ApprovalOutcome::Rule(
                "The action input is invalid; submit a valid tool call.".into(),
            ));
        }
        if t.policy_hooks.get(&call_id).is_some_and(|original| {
            original.get("tool_name") != req.get("tool_name")
                || original.get("input") != req.get("input")
        }) {
            let outcome = ApprovalOutcome::Rule(
                "The action input changed; submit a new action with unchanged input.".into(),
            );
            t.denied.lock().insert(call_id, outcome.clone());
            return answer(outcome);
        }
        let denied_outcome = t.denied.lock().get(&call_id).cloned();
        if let Some(outcome) = denied_outcome {
            return answer(outcome);
        }
        if self.stopped_here().is_some() {
            return answer(ApprovalOutcome::Stopped);
        }
        let decision = self.claude_policy(rt, &t.cwd, &tool, &input, &req);
        // Reused ids cannot replace the input that a person approved.
        if let Some(gate) = t.gates.get(&call_id)
            && (gate.tool != tool || gate.input != input || gate.mode != mode)
        {
            return answer(ApprovalOutcome::Rule(
                "The action or permissions changed; submit new work under the current mode.".into(),
            ));
        }
        let (reason, key) = match decision {
            Policy::Deny(msg) => {
                let outcome = ApprovalOutcome::Rule(msg);
                t.denied.lock().insert(call_id, outcome.clone());
                return answer(outcome);
            }
            Policy::Allow => return answer(ApprovalOutcome::Allowed(ApprovalDecision::Allow)),
            Policy::Ask { reason, key } => (reason, key),
        };
        let keys = self.session_keys(&rt.st.lock(), &key);
        let once = tool == "Bash"
            && self.config.read().security.sandbox == crate::config::SandboxMode::Auto
            && !crate::sandbox::support().complete();
        if !once && Self::session_allowed(&rt.st.lock(), &keys) {
            return answer(ApprovalOutcome::Allowed(ApprovalDecision::Allow));
        }
        let mut result = if let Some(gate) = t.gates.get(&call_id) {
            gate.result.clone()
        } else {
            let (send, result) = tokio::sync::watch::channel(None);
            let request = ApprovalRequest {
                id: new_id(),
                call_id: call_id.clone(),
                tool: tool.clone(),
                summary: crate::util::visible_controls_lines(&cc::summary(&input, &t.cwd)),
                reason: crate::util::visible_controls(&reason),
                arguments: input.clone(),
                once,
            };
            let token = t.cancel.child_token();
            t.approvals.insert(approval_id, token.clone());
            t.gates.insert(
                call_id.clone(),
                PermissionGate {
                    tool: tool.clone(),
                    input: input.clone(),
                    mode,
                    result: result.clone(),
                },
            );
            let (me, rt, denied) = (self.clone(), rt.clone(), t.denied.clone());
            let cwd = t.cwd.clone();
            let shown_on = rt.st.lock().approvals_on.clone();
            let stops_at = stops_at.clone();
            let (approved_tool, approved_input, approved_req, approved_call) =
                (tool.clone(), input.clone(), req.clone(), call_id.clone());
            tokio::spawn(async move {
                let (tool, input, req, call_id) =
                    (approved_tool, approved_input, approved_req, approved_call);
                // Its parent may answer for it where this policy would run it
                // unasked in the parent's mode, or the user allowed it there for
                // the session (checked again when the parent answers).
                let authority = helper.then(|| super::helper_approvals::ParentAuthority {
                    unasked: super::helper_approvals::ParentAuthority::MODES
                        .into_iter()
                        .filter(|m| {
                            matches!(
                                me.claude_policy_as(&rt, &cwd, &tool, &input, &req, Some(*m)),
                                Policy::Allow
                            )
                        })
                        .collect(),
                    keys: keys.clone(),
                });
                let mut outcome = me
                    .ask_approval_as(&rt, &shown_on, request, &token, authority)
                    .await;
                if !me.running_here_since(&stops_at) {
                    outcome = ApprovalOutcome::Stopped;
                } else if outcome.allowed() {
                    outcome = if me.effective_permission(&rt.st.lock()) != mode {
                        ApprovalOutcome::Rule(
                            "Permissions changed; submit new work under the current mode.".into(),
                        )
                    } else if let Policy::Deny(why) =
                        me.claude_policy(&rt, &cwd, &tool, &input, &req)
                    {
                        ApprovalOutcome::Rule(why)
                    } else if token.is_cancelled() {
                        ApprovalOutcome::Dismissed
                    } else {
                        outcome
                    };
                }
                if outcome == ApprovalOutcome::Allowed(ApprovalDecision::AllowSession) && !once {
                    Inner::allow_for_session(&mut rt.st.lock(), keys);
                    me.save(&rt);
                }
                if !outcome.allowed() {
                    denied.lock().insert(call_id, outcome.clone());
                }
                let _ = send.send(Some(outcome));
            });
            result
        };
        let (me, rt, denied, cancel, cwd) = (
            self.clone(),
            rt.clone(),
            t.denied.clone(),
            t.cancel.clone(),
            t.cwd.clone(),
        );
        tokio::spawn(async move {
            while result.borrow().is_none() {
                if result.changed().await.is_err() {
                    break;
                }
            }
            let mut outcome = result
                .borrow()
                .clone()
                .unwrap_or(ApprovalOutcome::Dismissed);
            if !me.running_here_since(&stops_at) {
                outcome = ApprovalOutcome::Stopped;
            } else if outcome.allowed() {
                outcome = if me.effective_permission(&rt.st.lock()) != mode {
                    ApprovalOutcome::Rule(
                        "Permissions changed; submit new work under the current mode.".into(),
                    )
                } else if let Policy::Deny(why) = me.claude_policy(&rt, &cwd, &tool, &input, &req) {
                    ApprovalOutcome::Rule(why)
                } else if cancel.is_cancelled() {
                    ApprovalOutcome::Dismissed
                } else {
                    outcome
                };
            }
            if !outcome.allowed() {
                denied.lock().insert(call_id, outcome.clone());
            }
            // Deny, expiry and policy refusal are tool results, not a Stop.
            // Only the real turn cancellation path sends `interrupt`.
            answer(outcome);
        });
    }

    /// Claude Code's `AskUserQuestion`: its questions go to the user as
    /// Tessera's own question cards, on one whiteboard in the chat (in every
    /// mode: a question isn't a permission), and the answers go back the way
    /// Claude Code takes them. Declined or unanswered: it is told so and
    /// carries on without.
    fn claude_ask_user(
        self: &Arc<Self>,
        rt: &Arc<Rt>,
        t: &mut Turn,
        request_id: String,
        req: Value,
        tx: mpsc::UnboundedSender<String>,
        cancel: &CancellationToken,
    ) {
        let input = req.get("input").cloned().unwrap_or(Value::Null);
        let Some(call) = req
            .get("tool_use_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            let _ = tx.send(cc::permission_line(
                &request_id,
                false,
                &input,
                "Claude Code did not identify this question; submit a valid tool call.",
            ));
            return;
        };
        if t.questions.get(call).is_some_and(|q| q.input != input) {
            let _ = tx.send(cc::permission_line(
                &request_id,
                false,
                &input,
                "The question changed; submit it with a new call id.",
            ));
            return;
        }
        if !t.questions.contains_key(call) {
            let qs = cc::questions(&input);
            if qs.is_empty() {
                let _ = tx.send(cc::permission_line(
                    &request_id,
                    false,
                    &input,
                    "There was no question to ask.",
                ));
                return;
            }
            if let Err(why) =
                self.reserve_helper_step(rt, "AskUserQuestion", &input, Some(call), false)
            {
                let _ = tx.send(cc::permission_line(&request_id, false, &input, why));
                return;
            }
            let owner = Arc::new(Mutex::new(super::helper_limits::Provisional::new(
                self, rt, call,
            )));
            let (send, result) = tokio::sync::watch::channel(None);
            let token = cancel.child_token();
            let waiters = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            t.questions.insert(
                call.to_string(),
                QuestionGate {
                    input: input.clone(),
                    result,
                    token: token.clone(),
                    waiters: waiters.clone(),
                    owner: owner.clone(),
                },
            );
            let (me, sid, input) = (self.clone(), t.sid.clone(), input.clone());
            tokio::spawn(async move {
                let outcome = async {
                    let title = qs
                        .iter()
                        .find(|q| !q.header.is_empty())
                        .map_or("A question", |q| q.header.as_str());
                    let markdown = |answers: &[String]| BoardContent::Markdown {
                        markdown: cc::questions_markdown(&qs, answers),
                    };
                    let board = me
                        .show_board(&sid, None, Some(title), markdown(&[]), false)
                        .map_err(|e| format!("The question couldn't be shown: {e}."))?
                        .id;
                    let mut answers = Vec::new();
                    for q in &qs {
                        answers.push(me.clone().claude_answer(&sid, &board, q, &token).await?);
                        let _ = me.show_board(&sid, Some(&board), None, markdown(&answers), false);
                    }
                    if token.is_cancelled()
                        || waiters.load(std::sync::atomic::Ordering::SeqCst) == 0
                    {
                        return Err("The question was cancelled.".into());
                    }
                    // The shared owner stays provisional until a waiter
                    // successfully queues the affirmative reply.
                    Ok(cc::answered_input(&input, &qs, &answers))
                }
                .await;
                let _ = send.send(Some(outcome));
            });
        }
        let gate = &t.questions[call];
        let mut result = gate.result.clone();
        let shared_token = gate.token.clone();
        let waiters = gate.waiters.clone();
        let owner = gate.owner.clone();
        waiters.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let token = cancel.child_token();
        t.approvals.insert(request_id.clone(), token.clone());
        tokio::spawn(async move {
            let _waiter = QuestionWaiter {
                waiters,
                token: shared_token,
                owner: owner.clone(),
            };
            let outcome = loop {
                if token.is_cancelled() {
                    break Err("This question callback was cancelled.".to_string());
                }
                if let Some(outcome) = result.borrow().clone() {
                    break outcome;
                }
                tokio::select! {
                    _ = token.cancelled() => {},
                    changed = result.changed() => if changed.is_err() {
                        break Err("The question ended without an answer.".to_string());
                    },
                }
            };
            match outcome {
                Ok(answered) => {
                    if token.is_cancelled() {
                        let _ = tx.send(cc::permission_line(
                            &request_id,
                            false,
                            &input,
                            "This question callback was cancelled.",
                        ));
                    } else if let Err(why) = owner.lock().release_to(|| {
                        tx.send(cc::permission_line(&request_id, true, &answered, ""))
                            .is_ok()
                    }) {
                        let _ = tx.send(cc::permission_line(&request_id, false, &input, why));
                    }
                }
                Err(why) => {
                    let _ = tx.send(cc::permission_line(&request_id, false, &input, &why));
                }
            }
        });
    }

    /// The user's answer to one of Claude Code's questions: the option picked
    /// (several joined with ", "), and what they typed for "Something else".
    async fn claude_answer(
        self: Arc<Self>,
        sid: &str,
        board: &str,
        q: &cc::Question,
        token: &CancellationToken,
    ) -> Result<String, String> {
        const DECLINED: &str = "The user chose not to answer.";
        let mut options: Vec<String> = q.options.iter().map(|o| o.0.clone()).collect();
        options.push(cc::OTHER.to_string());
        let other = options.len() - 1;
        let input = AskInput::Choice {
            options: options.clone(),
            multi: q.multi,
        };
        let asked = self
            .clone()
            .ask_board(
                sid.into(),
                Some(board.into()),
                None,
                q.text.clone(),
                input,
                false,
                token.clone(),
            )
            .await;
        let picked: Vec<usize> = match asked {
            Ok(AskAnswer::Choice { index }) => vec![index as usize],
            Ok(AskAnswer::Choices { indexes }) => indexes.into_iter().map(|i| i as usize).collect(),
            Ok(AskAnswer::Cancel) => return Err(DECLINED.into()),
            Ok(_) => return Err("The answer didn't fit the question.".into()),
            Err(e) => return Err(format!("No answer: {e}.")),
        };
        let mut parts: Vec<String> = picked
            .iter()
            .filter(|i| **i != other)
            .filter_map(|i| options.get(*i).cloned())
            .collect();
        if picked.contains(&other) {
            let typed = AskInput::Text {
                multiline: false,
                placeholder: String::new(),
            };
            let prompt = format!("Your answer: {}", q.text);
            match self
                .ask_board(
                    sid.into(),
                    Some(board.into()),
                    None,
                    prompt,
                    typed,
                    false,
                    token.clone(),
                )
                .await
            {
                Ok(AskAnswer::Text { text }) if !text.trim().is_empty() => {
                    parts.push(text.trim().to_string())
                }
                Ok(AskAnswer::Cancel) => return Err(DECLINED.into()),
                Ok(_) => {}
                Err(e) => return Err(format!("No answer: {e}.")),
            }
        }
        if parts.is_empty() {
            return Err(DECLINED.into());
        }
        Ok(parts.join(", "))
    }

    /// Authorization is separate from lean/small schema loading. This includes
    /// definition, depth, bot, read-only and final-report ceilings.
    pub(super) fn claude_capabilities(&self, st: &RtState) -> Vec<String> {
        if st.no_tool_calls {
            return Vec::new();
        }
        self.base_tool_specs(st)
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    /// CLI-owned integrations are capabilities of unrestricted chats. Narrow
    /// actors need an explicit native MCP-tool entry in every ceiling. No
    /// schema/tool-name guess is authority to widen a bot or report-only run.
    fn claude_external_allowed(&self, st: &RtState, tool: Option<&str>) -> bool {
        if !self.redaction_secrets().is_empty() {
            return false;
        }
        if st.info.private || st.info.internal || st.info.lean || st.no_tool_calls {
            return false;
        }
        let admits = |names: &[String]| match tool {
            Some(tool) => names.iter().any(|n| n == tool),
            None => names
                .iter()
                .any(|n| n.starts_with("mcp__") && !n.starts_with(cc::PREFIX)),
        };
        if let Some(only) = &st.tools_only {
            let names: Vec<_> = only.iter().map(|n| n.to_string()).collect();
            if !admits(&names) {
                return false;
            }
        }
        if let Some(bot) = self.speaker_bot(st)
            && let Some(names) = crate::bots::allowed_tools(&bot)
            && !admits(&names)
        {
            return false;
        }
        if let Some(tool) = tool
            && self.registry.read().get(tool).is_some()
            && !self.claude_capabilities(st).iter().any(|n| n == tool)
        {
            return false;
        }
        true
    }

    fn claude_external_read_only(&self, tool: &str, req: &Value) -> bool {
        let Some(provenance) = req.get("mcp_server") else {
            return false;
        };
        let Some(server) = provenance.get("name").and_then(Value::as_str) else {
            return false;
        };
        let identity_matches = self.config.read().mcp_servers.iter().any(|cfg| {
            let name: String = cfg
                .name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            cfg.enabled
                && cfg.name == server
                && tool.starts_with(&format!("mcp__{name}__"))
                && match provenance.get("type").and_then(Value::as_str) {
                    Some("stdio") => {
                        cfg.command.is_some()
                            && cfg.command.as_deref()
                                == provenance.get("command").and_then(Value::as_str)
                            && provenance.get("args").cloned().unwrap_or_else(|| json!([]))
                                == json!(cfg.args)
                    }
                    Some("http" | "sse") => {
                        cfg.url.as_deref() == provenance.get("url").and_then(Value::as_str)
                            && cfg.url.is_some()
                    }
                    _ => false,
                }
        });
        identity_matches
            && self
                .registry
                .read()
                .get(tool)
                .is_some_and(|t| t.kind() == crate::tools::ToolKind::Read)
    }

    fn claude_external_policy(
        &self,
        rt: &Rt,
        tool: &str,
        mode: PermissionMode,
        clearly_read_only: bool,
    ) -> Policy {
        if !self.claude_external_allowed(&rt.st.lock(), Some(tool)) {
            return Policy::Deny(
                "This external integration is not available to this actor.".into(),
            );
        }
        match mode {
            PermissionMode::ReadOnly if !clearly_read_only => Policy::Deny(
                "This external integration has no trusted read-only classification; the action was not performed.".into()),
            PermissionMode::ReadOnly | PermissionMode::Auto => Policy::Allow,
            PermissionMode::Default | PermissionMode::Ask => Policy::Ask {
                reason: format!("`{tool}` is an external integration managed by Claude Code."),
                key: format!("mcp:{tool}"),
            },
        }
    }

    fn claude_sync_permission(
        &self,
        rt: &Rt,
        t: &mut Turn,
        tx: Option<&mpsc::UnboundedSender<String>>,
    ) {
        let mode = self.effective_permission(&rt.st.lock());
        if mode == t.mode {
            return;
        }
        t.mode = mode;
        // Never let a response to an old card release a pending mutation.
        for token in t.approvals.values() {
            token.cancel();
        }
        if let Some(tx) = tx {
            let _ = tx.send(cc::set_permission_mode_line(&new_id(), mode));
        }
    }

    fn claude_policy(
        self: &Arc<Self>,
        rt: &Rt,
        cwd: &Path,
        tool: &str,
        input: &Value,
        req: &Value,
    ) -> Policy {
        self.claude_policy_as(rt, cwd, tool, input, req, None)
    }

    /// [`claude_policy`](Self::claude_policy) as it would decide in `mode`
    /// (else the chat's own): whether the chat that started a helper could
    /// run the helper's action unasked.
    fn claude_policy_as(
        self: &Arc<Self>,
        rt: &Rt,
        cwd: &Path,
        tool: &str,
        input: &Value,
        req: &Value,
        mode_as: Option<PermissionMode>,
    ) -> Policy {
        let (mut mode, allowed, mut depth, sid) = {
            let st = rt.st.lock();
            if st.no_tool_calls {
                return Policy::Deny(super::helper_limits::STEP_EXHAUSTED.into());
            }
            (
                self.effective_permission(&st),
                self.claude_capabilities(&st),
                st.depth,
                st.info.id.clone(),
            )
        };
        if let Some(m) = mode_as {
            mode = m;
        }
        // Documented hook metadata may only narrow host authority. In
        // particular, a CLI-native child cannot regain spawning or bypass its
        // own plan/exploration floor through a host permission response.
        if actor_is_read_only(req) {
            mode = PermissionMode::ReadOnly;
        }
        if req.get("agent_id").and_then(Value::as_str).is_some() {
            depth = depth.max(1);
        }
        if depth > 0
            && (matches!(tool, "Agent" | "Task" | "Workflow" | "mcp__tessera__agent")
                || (tool == "mcp__tessera__agents"
                    && input.get("action").and_then(Value::as_str) == Some("start")))
        {
            return Policy::Deny("Helpers cannot start more helpers.".into());
        }
        // The bridge checks again at dispatch, so no duplicate card here.
        if let Some(native) = cc::native_name(tool) {
            if native == "schedule"
                && let Err(why) = crate::tools::meta::schedule_action(input)
            {
                return Policy::Deny(why.into());
            }
            if delegation_tool(native, input) && (!main_actor(req) || depth > 0) {
                return Policy::Deny(
                    "Delegation is available only to the main chat, not CLI helpers.".into(),
                );
            }
            if mode == PermissionMode::ReadOnly
                && self.registry.read().get(native).is_some_and(|tool| {
                    matches!(
                        tool.kind(),
                        crate::tools::ToolKind::Write | crate::tools::ToolKind::Exec
                    ) || matches!(
                        (native, input.get("action").and_then(Value::as_str)),
                        ("machines", Some("run" | "write" | "edit")) | ("lsp", Some("rename"))
                    )
                })
            {
                return Policy::Deny(
                    "This helper is read-only; the action was not performed.".into(),
                );
            }
            return if allowed.iter().any(|n| n == native) {
                Policy::Allow
            } else {
                Policy::Deny("This tool is not available to this actor now.".into())
            };
        }
        if matches!(tool, "Agent" | "Task")
            && input.get("isolation").and_then(Value::as_str) == Some("remote")
        {
            return Policy::Deny("Cloud-executed helpers are not available through Tessera's local permission contract.".into());
        }
        if tool.starts_with("mcp__") {
            // The public hook input carries server provenance, not tool
            // annotations. Never trust a model's readOnlyHint or a name like
            // read_file. Only a locally registered classification is known.
            let read_only = self.claude_external_read_only(tool, req);
            return self.claude_external_policy(rt, tool, mode, read_only);
        }
        // This is rechecked for every call, including a kept process or helper
        // after a secret was saved mid-turn. Hooks cannot remove original native
        // output from the CLI's telemetry, even where output rewriting exists.
        if !self.redaction_secrets().is_empty() && cc::unredacted_tool(tool) {
            return Policy::Deny("Use Tessera's bridged tools for redacted results (mcp__tessera__read, mcp__tessera__grep, mcp__tessera__bash). Native content tools are unavailable while stored secrets exist.".into());
        }
        let limits = cc::limits_for(Some(&allowed));
        if !limits.only.contains(&tool) {
            return Policy::Deny(format!(
                "`{tool}` is not available to this actor; the action was not performed."
            ));
        }
        if matches!(
            tool,
            "ToolSearch" | "AskUserQuestion" | "EnterPlanMode" | "ExitPlanMode"
        ) {
            return if tool == "ExitPlanMode" && mode == PermissionMode::ReadOnly {
                Policy::Deny("This chat is read-only: give the plan as your answer instead.".into())
            } else {
                Policy::Allow
            };
        }
        if matches!(tool, "ScheduleWakeup" | "CronCreate" | "CronDelete")
            && mode == PermissionMode::ReadOnly
        {
            return Policy::Deny(
                "This chat is read-only: scheduled actions cannot be changed.".into(),
            );
        }
        if tool == "Workflow" && !allowed.iter().any(|n| n == "bash") {
            return Policy::Deny(
                "Workflows execute a local script; this actor has no command capability.".into(),
            );
        }
        if matches!(tool, "Workflow" | "SendMessage")
            && (input["isolation"] == "remote" || input["machine"] == "remote")
        {
            return Policy::Deny("Cloud or remote execution is not available through this local permission contract.".into());
        }
        let Some((_, native)) = cc::BUILTINS.iter().find(|(name, _)| *name == tool) else {
            return Policy::Deny("Unsupported tool; the action was not performed.".into());
        };
        let Some(assessor) = self.registry.read().get(native) else {
            return Policy::Deny("No assessment is available for this tool.".into());
        };
        let mut args = input.clone();
        let path = [
            "file_path",
            "notebook_path",
            "filePath",
            "path",
            "scriptPath",
            "local_path",
        ]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str));
        if let Some(path) = path {
            args["path"] = json!(path);
        }
        // CLI cwd can move after Bash or EnterWorktree; never resolve a later
        // relative action in the old directory or omit command cwd assessment.
        if let Some(current) = req.get("cwd").and_then(Value::as_str) {
            args["cwd"] = json!(current);
            if let Some(path) = path {
                args["path"] = json!(Path::new(current).join(path));
            }
        }
        match tool {
            // A WebSocket still uses the command capability's cwd/path floor,
            // without pretending its connection text is a shell command.
            "Monitor" if input.get("ws").is_some() => args["command"] = json!(""),
            "CronCreate" => args["action"] = json!("create"),
            "CronList" => args["action"] = json!("list"),
            "CronDelete" => args["action"] = json!("delete"),
            "ScheduleWakeup" => {
                args["action"] = json!(if input["stop"] == true {
                    "delete"
                } else {
                    "update"
                })
            }
            "Workflow" => args["action"] = json!("start"),
            "ListAgents" => args["action"] = json!("list"),
            "SendMessage" => args["action"] = json!("continue"),
            "EnterWorktree" | "ExitWorktree" if path.is_none() => {
                args["path"] = json!(
                    req.get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| cwd.to_str().unwrap_or("."))
                )
            }
            "ShareOnboardingGuide" => args["path"] = json!(cwd.join("ONBOARDING.md")),
            _ => {}
        }
        let ctx = ToolCtx {
            session_id: sid,
            call_id: String::new(),
            seq: 0,
            cwd: req
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.into()),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(self.clone(), mode == PermissionMode::ReadOnly)),
            progress: Arc::new(|_| {}),
            depth,
            cleared: None,
            admission: self.action_admission(&rt.st.lock()),
        };
        if tool == "Workflow" && path.is_none() {
            return Policy::Deny("A workflow script path is required for assessment.".into());
        }
        if tool == "EnterWorktree"
            && input
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.contains(['/', '\\']) || matches!(n, "." | ".."))
        {
            return Policy::Deny("A worktree name must be a name, not a path.".into());
        }
        if matches!(
            tool,
            "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit"
        ) && path.is_none()
        {
            return Policy::Deny("The file action has no path; it was not performed.".into());
        }
        if matches!(tool, "Bash" | "PowerShell") || (tool == "Monitor" && input.get("ws").is_none())
        {
            if input.get("command").and_then(Value::as_str).is_none() {
                return Policy::Deny("The command is missing; it was not performed.".into());
            }
            if tool == "PowerShell"
                && let Risk::Block { reason } = crate::tools::policy::assess_command_for(
                    input["command"].as_str().unwrap_or_default(),
                    true,
                )
            {
                return Policy::Deny(reason);
            }
        }
        if cc::credential_target(tool, input, &ctx.cwd) {
            return Policy::Deny("Claude Code's native tool cannot safely return credential files. Use Tessera's bridged `read` (mcp__tessera__read), or bridged `grep` for a folder search; Tessera redacts known secrets before returning the result.".into());
        }
        if let Some(path) = args.get("path").and_then(Value::as_str)
            && let Risk::Block { reason } = crate::tools::path_risk(&ctx.resolve(path), &ctx)
        {
            return Policy::Deny(reason);
        }
        // These built-ins do not exclude protected descendants as Tessera's
        // search walkers do. Refuse an unsafe search root rather than leak it.
        if matches!(tool, "Grep" | "Glob") {
            let root = crate::tools::canonical(
                &path.map(|p| ctx.resolve(p)).unwrap_or_else(|| cwd.into()),
            );
            if self
                .protected_paths()
                .roots
                .iter()
                .any(|p| p.starts_with(&root))
            {
                return Policy::Deny(
                    "This search could read Tessera's protected store; choose a narrower folder."
                        .into(),
                );
            }
        }
        let mut risk = if tool == "Artifact" && input["action"] == "list" {
            Risk::None
        } else {
            crate::tools::permission_risk(assessor.as_ref(), &args, &ctx, mode)
        };
        // Commands in PowerShell need Windows rules even on a Unix server
        // with pwsh installed. Native Bash's protected paths/secret checks
        // above still apply, including before YOLO skips ordinary questions.
        if tool == "PowerShell"
            && let Risk::Ask { reason, key } = crate::tools::policy::assess_command_for(
                input["command"].as_str().unwrap_or_default(),
                true,
            )
            && !matches!(risk, Risk::Block { .. })
            && mode != PermissionMode::Auto
        {
            // Windows rules add requirements; they cannot erase native
            // secret/domain checks or grants. Ordinary Ask-mode exec keys are
            // replaced by the risky-rule keys, as native command policy does.
            risk = match risk {
                Risk::Ask {
                    reason: native_reason,
                    key: native_key,
                } if !native_key.starts_with("exec:") => Risk::Ask {
                    reason: format!("{reason} {native_reason}"),
                    key: format!("{key}+{native_key}"),
                },
                _ => Risk::Ask { reason, key },
            };
        }
        if tool == "Monitor"
            && let Some(ws) = input.get("ws")
        {
            if input.get("command").is_some() {
                return Policy::Deny(
                    "A monitor needs either a command or a WebSocket, not both.".into(),
                );
            }
            let Some(url) = ws.get("url").and_then(Value::as_str) else {
                return Policy::Deny("The WebSocket address is missing.".into());
            };
            let Ok(parsed) = url::Url::parse(url) else {
                return Policy::Deny("Invalid WebSocket address.".into());
            };
            if !matches!(parsed.scheme(), "ws" | "wss")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Policy::Deny(
                    "A WebSocket address must use ws/wss without embedded credentials.".into(),
                );
            }
            if !matches!(risk, Risk::Block { .. }) {
                risk = if mode == PermissionMode::Auto {
                    Risk::None
                } else {
                    Risk::Ask {
                        reason: "opens a background WebSocket connection".into(),
                        key: format!("monitor:{url}"),
                    }
                };
            }
        }
        if matches!(
            tool,
            "ListMcpResourcesTool"
                | "ReadMcpResourceTool"
                | "ReadMcpResourceDirTool"
                | "RefreshMcpTools"
                | "WaitForMcpServers"
        ) {
            let st = rt.st.lock();
            let server = input.get("server").and_then(Value::as_str).unwrap_or("*");
            let resource_tool = format!("mcp__{server}__{tool}");
            if !self.claude_external_allowed(&st, Some(&resource_tool)) && server != cc::SERVER {
                return Policy::Deny(
                    "This actor cannot use Claude Code's external MCP integrations.".into(),
                );
            }
            drop(st);
            if tool != "WaitForMcpServers"
                && input.get("server").and_then(Value::as_str) != Some(cc::SERVER)
            {
                risk = match mode {
                    PermissionMode::ReadOnly | PermissionMode::Auto => Risk::None,
                    _ => Risk::Ask {
                        reason: "uses an external MCP integration".into(),
                        key: format!(
                            "mcp:{tool}:{}",
                            input.get("server").and_then(Value::as_str).unwrap_or("*")
                        ),
                    },
                };
            }
        }
        if tool == "ExitWorktree"
            && (input["action"] == "remove" || input["discard_changes"] == true)
            && !matches!(risk, Risk::Block { .. })
            && mode != PermissionMode::Auto
        {
            risk = Risk::Ask {
                reason: "removes a git worktree and possibly discards changes".into(),
                key: format!("worktree-remove:{}", args["path"]),
            };
        }
        if matches!(
            tool,
            "Workflow"
                | "SendMessage"
                | "ShowOnboardingRolePicker"
                | "SendFeedback"
                | "Artifact"
                | "SendUserFile"
                | "ShareOnboardingGuide"
                | "Projects"
        ) {
            let read_only = tool == "Artifact" && input["action"] == "list"
                || tool == "Projects"
                    && matches!(
                        input["method"].as_str(),
                        Some("project_info" | "project_read" | "project_search")
                    );
            if mode == PermissionMode::ReadOnly && !read_only {
                return Policy::Deny("This action can execute work or change state outside the read-only conversation.".into());
            }
            if !matches!(risk, Risk::Block { .. }) && !read_only && mode != PermissionMode::Auto {
                risk = Risk::Ask {
                    reason: format!(
                        "`{tool}` starts helpers, changes state or delivers content outside this conversation"
                    ),
                    key: format!("claude-action:{tool}:{input}"),
                };
            }
        }
        match risk {
            Risk::Block { reason } => Policy::Deny(reason),
            Risk::Ask { reason, key } => Policy::Ask { reason, key },
            Risk::None
                if req.get("classifier_approvable").and_then(Value::as_bool) == Some(false) =>
            {
                Policy::Ask {
                    reason: req
                        .get("decision_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("Claude Code asks before this action.")
                        .into(),
                    key: format!("claude-action:{tool}:{input}"),
                }
            }
            Risk::None => Policy::Allow,
        }
    }

    /// The turn's usage: one usage row per model with the API-equivalent cost
    /// Claude Code reported, and the session's totals. Both come from what each
    /// model used since the last result (`modelUsage` holds running totals, and
    /// one run answers queued messages one after another), so nothing counts twice.
    fn claude_usage(&self, rt: &Rt, t: &mut Turn, v: &Value) {
        let (kind, sid) = {
            let mut st = rt.st.lock();
            // The window of the model this chat runs. Claude Code also reports the small
            // model it uses for quick tasks (200K), which comes first in the map.
            if let Some(w) = v.get("modelUsage").and_then(|m| {
                cc::reported_window(m, t.resolved.as_deref().unwrap_or(&t.model.model))
            }) {
                st.info.context.window = w;
            }
            (Self::usage_kind(&st.info), t.sid.clone())
        };
        let used: Vec<(String, Usage, Option<f64>)> = match v
            .get("modelUsage")
            .and_then(Value::as_object)
            .filter(|m| !m.is_empty())
        {
            Some(models) => {
                let (rows, totals) = cc::usage_since(models, &t.reported);
                t.reported = totals;
                rows.into_iter()
                    .map(|u| (u.model, u.usage, Some(u.cost)))
                    .collect()
            }
            // Without it: this turn's tokens, priced from the price list.
            None => vec![(
                t.resolved.clone().unwrap_or_else(|| t.model.model.clone()),
                cc::usage(v.get("usage").unwrap_or(&Value::Null)),
                None,
            )],
        };
        {
            let mut st = rt.st.lock();
            let u = &mut st.info.usage;
            for (_, x, _) in &used {
                u.input += x.input;
                u.output += x.output;
                u.cache_read += x.cache_read;
                u.cache_write += x.cache_write;
            }
        }
        for (model, usage, cost) in used {
            let m = ModelRef {
                provider: t.model.provider.clone(),
                model,
            };
            self.log_usage_reported(Some(&sid), &m, kind, &usage, cost);
        }
    }

    fn claude_agent_start(&self, rt: &Rt, t: &mut Turn, call: &str, args: &Value) {
        if t.background_ended(None, Some(call)) {
            return;
        }
        let a = t.agents.entry(call.to_string()).or_default();
        if a.started {
            return;
        }
        a.started = true;
        let arg = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        if let Some(d) = arg("description") {
            a.description = d.to_string();
        }
        if let Some(k) = arg("subagent_type") {
            a.kind = k.to_string();
        }
        if args.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            a.requested_background = true;
        }
        let (child, author) = {
            let pst = rt.st.lock();
            if pst.info.private {
                return;
            }
            let name = if a.description.is_empty() {
                "Subagent"
            } else {
                &a.description
            };
            let bot = self.speaker_bot(&pst);
            match self.create_session(
                Some(pst.info.cwd.clone()),
                Some(pst.info.model.clone()),
                Some(format!("↳ {}", one_line(name, 80))),
                SessionKind::Subagent,
                Some(&pst),
                None,
                bot.as_ref(),
            ) {
                // Its task is the main agent's words, not the user's.
                Ok(c) => (c, super::agent_author(&pst)),
                Err(e) => {
                    tracing::warn!("no session for a Claude Code subagent: {e}");
                    return;
                }
            }
        };
        {
            let mut cst = child.st.lock();
            cst.info.parent_call = Some(call.to_string());
            cst.info.helper_owner = Some(HelperOwner::ClaudeCode);
            // Without a type Claude Code runs its general-purpose agent.
            cst.info.role = Some(if a.kind.is_empty() {
                "general-purpose".to_string()
            } else {
                a.kind.clone()
            });
        }
        let task = arg("prompt").unwrap_or("(no task given)");
        let _ = self.append(
            &child,
            EntryBody::User {
                content: vec![ContentPart::text(task)],
                author: Some(author),
            },
        );
        self.set_status(&child, SessionStatus::Running);
        a.child = Some(child);
    }

    /// A message of one of Claude Code's subagents, for its session (it arrives
    /// block by block, like the conversation's own).
    fn claude_agent_message(
        &self,
        t: &mut Turn,
        call: &str,
        msg: &Value,
        parts: Vec<AssistantPart>,
    ) {
        let model = t.resolved.clone().unwrap_or_else(|| t.model.model.clone());
        let Some(a) = t
            .agents
            .get_mut(call)
            .filter(|a| a.child.is_some() && !a.done)
        else {
            return;
        };
        let id = msg
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if a.pending.as_ref().is_some_and(|p| p.id != id) {
            self.claude_agent_flush(a, &t.model.provider, &model);
        }
        if let Some(m) = msg.get("model").and_then(Value::as_str) {
            a.model = Some(m.to_string());
        }
        let p = a.pending.get_or_insert_with(|| Pending {
            id,
            usage: cc::usage(msg.get("usage").unwrap_or(&Value::Null)),
            ..Default::default()
        });
        for part in parts.into_iter().map(shown_part) {
            if let AssistantPart::ToolCall { id, name, .. } = &part {
                a.open.push((id.clone(), name.clone()));
            }
            p.parts.push(part);
        }
    }

    /// Tool results of one of Claude Code's subagents, for its session (and
    /// its edits and checks for the chat's).
    fn claude_agent_results(self: &Arc<Self>, t: &mut Turn, call: &str, v: &Value) {
        let done = self.claude_agent_results_shown(t, call, v);
        for (id, name, is_error) in done {
            self.claude_tool_done(t, &id, &name, is_error);
            let block = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|b| b.get("tool_use_id").and_then(Value::as_str) == Some(&id))
                });
            let mut out = ToolOutput {
                content: cc::tool_result_parts(
                    block.and_then(|b| b.get("content")).unwrap_or(&Value::Null),
                ),
                is_error,
                details: None,
            };
            if let Some(real) = t.bridge.as_ref().and_then(|b| b.done.lock().remove(&id)) {
                out = real;
            }
            self.claude_observe(t, &id, &name, &out);
        }
    }

    fn claude_agent_results_shown(
        &self,
        t: &mut Turn,
        call: &str,
        v: &Value,
    ) -> Vec<(String, String, bool)> {
        let mut done = Vec::new();
        let model = t.resolved.clone().unwrap_or_else(|| t.model.model.clone());
        let cwd = t.cwd.clone();
        let Some(a) = t
            .agents
            .get_mut(call)
            .filter(|a| a.child.is_some() && !a.done)
        else {
            return done;
        };
        self.claude_agent_flush(a, &t.model.provider, &model);
        let Some(child) = a.child.clone() else {
            return done;
        };
        let blocks = v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for b in blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        {
            let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let name = match a.open.iter().position(|(c, _)| c == id) {
                Some(i) => a.open.remove(i).1,
                None => continue,
            };
            let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            done.push((id.to_string(), name.clone(), is_error));
            let mut content = cc::tool_result_parts(b.get("content").unwrap_or(&Value::Null));
            self.redact_parts(&mut content);
            let details = (!is_error && matches!(name.as_str(), "Edit" | "MultiEdit" | "Write"))
                .then(|| {
                    v.get("tool_use_result").cloned().and_then(|mut result| {
                        self.redact_metadata(&mut result);
                        cc::edit_details(&result, &cwd)
                    })
                })
                .flatten()
                .or_else(|| {
                    v.get("tool_use_result")
                        .and_then(|r| cc::shell_details(&name, r))
                });
            if !is_error && let Some(args) = t.call_args.get(id) {
                self.claude_task_result(&child, &name, args, v);
            }
            let _ = self.append(
                &child,
                EntryBody::ToolResult {
                    call_id: id.to_string(),
                    tool: name,
                    content,
                    is_error,
                    artifact: None,
                    details,
                },
            );
        }
        done
    }

    /// What a subagent said so far, into its session.
    fn claude_agent_flush(&self, a: &mut AgentRun, provider: &str, model: &str) {
        let (Some(child), Some(p)) = (a.child.clone(), a.pending.take()) else {
            return;
        };
        if p.parts.is_empty() {
            return;
        }
        let calls = p
            .parts
            .iter()
            .filter(|x| matches!(x, AssistantPart::ToolCall { .. }))
            .count() as u32;
        {
            let mut st = child.st.lock();
            st.info.steps += calls;
            let u = &mut st.info.usage;
            u.input += p.usage.input;
            u.output += p.usage.output;
            u.cache_read += p.usage.cache_read;
            u.cache_write += p.usage.cache_write;
        }
        let _ = self.append(
            &child,
            EntryBody::Assistant {
                content: p.parts,
                model: ModelRef {
                    provider: provider.to_string(),
                    model: a.model.clone().unwrap_or_else(|| model.to_string()),
                },
                usage: p.usage,
                stop: if calls > 0 {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                },
                error: None,
                author: None,
            },
        );
        self.save(&child);
    }

    /// One of Claude Code's subagents is done (or stopped): its session says so.
    fn claude_agent_end(
        &self,
        a: &mut AgentRun,
        provider: &str,
        model: &str,
        status: SessionStatus,
        why: Option<&str>,
    ) {
        if a.done {
            return;
        }
        a.done = true;
        self.claude_agent_flush(a, provider, model);
        let Some(child) = a.child.clone() else {
            return;
        };
        for (id, name) in std::mem::take(&mut a.open) {
            let _ = self.append(
                &child,
                EntryBody::ToolResult {
                    call_id: id,
                    tool: name,
                    content: vec![ContentPart::text("Claude Code ended before this finished.")],
                    is_error: true,
                    artifact: None,
                    details: None,
                },
            );
        }
        if let Some(why) = why {
            self.notice(&child, NoticeLevel::Info, why);
        }
        self.set_status(&child, status);
    }

    /// The model for Tessera's own small requests (titles, summaries, the
    /// memory review) when nothing else is set: the chat's own, except that
    /// a chat on Claude Code asks its smallest model ([`cc::UTILITY_MODEL`]).
    pub(super) fn utility_for(&self, own: ModelRef) -> ModelRef {
        if self.providers.kind_of(&own.provider) == Some(ProviderKind::ClaudeCode) {
            ModelRef {
                provider: own.provider,
                model: cc::UTILITY_MODEL.into(),
            }
        } else {
            own
        }
    }

    /// One answer from Claude Code (`claude -p`), for Tessera's own
    /// requests: no tools, no MCP servers, nothing kept, in an empty folder
    /// and the command sandbox. Counted as the plan's usage.
    pub(super) async fn claude_complete(
        &self,
        model: &ModelRef,
        system: &str,
        prompt: &str,
        cancel: &CancellationToken,
        tag: (&str, &str),
    ) -> anyhow::Result<(String, Usage)> {
        let account = self.claude_account(&model.provider);
        let path = cc::locate(&account.command).ok_or_else(|| {
            anyhow!(
                cc::info(
                    &cc::Detection::default(),
                    &crate::node::hostname(),
                    &model.provider,
                    &account,
                )
                .hint
            )
        })?;
        let me = self.me.upgrade().ok_or_else(|| anyhow!("shutting down"))?;
        let run = RunFiles::with_system(system)?;
        let directory =
            crate::tools::file_object::ProcessDirectory::open(&run.dir).map_err(|e| anyhow!(e))?;
        // A private chat's small requests leave the user's customizations out too.
        let private = match self.live_session(tag.0) {
            Some(rt) => rt.st.lock().info.private,
            None => self
                .store
                .get_session(tag.0)
                .ok()
                .flatten()
                .is_some_and(|(info, _)| info.private),
        };
        let mut args = cc::one_shot_args(&model.model, &run.system);
        if private {
            args.extend(cc::private_controls());
        }
        let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        let (mut cmd, held) = claude_command(
            &path,
            &account,
            args,
            directory,
            Host(me, false).sandbox().as_ref(),
            &self.config.read().agent.pass_env,
            false,
            private,
        )
        .map_err(|e| anyhow!(e))?;
        cmd.stderr(std::process::Stdio::null());
        let mut child = crate::platform::spawn(&mut cmd, crate::platform::Start::Agent)
            .map_err(|e| anyhow!(crate::tools::file_object::launch_error(&e)))?;
        let _held = held.after_spawn();
        let tree = crate::tools::bash::ChildTree::capture(child.id().unwrap_or(0));
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
        }
        let out = tokio::select! {
            o = child.wait_with_output() => o?,
            _ = cancel.cancelled() => {
                tree.stop();
                anyhow::bail!("cancelled");
            }
        };
        drop(run);
        let answer =
            cc::one_shot_result(&String::from_utf8_lossy(&out.stdout)).map_err(|e| anyhow!(e))?;
        tree.disarm();
        let m = ModelRef {
            provider: model.provider.clone(),
            model: answer.model.unwrap_or_else(|| model.model.clone()),
        };
        self.log_usage_reported(Some(tag.0), &m, tag.1, &answer.usage, answer.cost);
        Ok((answer.text, answer.usage))
    }
}

/// Shared by full and one-shot CLI launches, including their cwd binding.
#[allow(clippy::too_many_arguments)]
fn claude_command(
    path: &Path,
    account: &cc::Account,
    args: Vec<std::ffi::OsString>,
    directory: crate::tools::file_object::ProcessDirectory,
    spec: Option<&(crate::sandbox::Spec, bool)>,
    pass_env: &[String],
    todo_tools: bool,
    private: bool,
) -> Result<
    (
        tokio::process::Command,
        crate::tools::file_object::DirectoryHold,
    ),
    String,
> {
    let sandbox = directory.prepare(spec)?;
    let (program, args) = directory.argv(sandbox.as_ref(), path.to_path_buf().into(), args);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("TESSERA", "1")
        .kill_on_drop(true);
    if private {
        // Nothing of the user's or the project's customizations comes along.
        crate::tools::bash::pass_environment(&mut cmd, &cc::private_pass_env(pass_env));
        cmd.envs(cc::PRIVATE_ENV);
    } else {
        crate::tools::bash::pass_environment(&mut cmd, pass_env);
    }
    if todo_tools {
        cmd.env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1");
    }
    cmd.env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
    account.apply(&mut cmd);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            crate::tools::bash::yield_to_server();
            Ok(())
        });
    }
    let held = directory.apply(&mut cmd, sandbox);
    Ok((cmd, held))
}

/// A run to do again: `Afresh` when the conversation Claude Code continues
/// lost its history (start a new one, told what happened so far); `Again`
/// when the process kept from the last turn had ended before it took this
/// one (start a new process on the same conversation).
enum Retry {
    Afresh,
    Again,
}

/// A running `claude` and its pipes.
struct Proc {
    child: tokio::process::Child,
    _directory: Option<crate::tools::file_object::DirectoryHold>,
    #[cfg(windows)]
    windows_job: Option<crate::platform::windows::TreeJob>,
    #[cfg(target_os = "linux")]
    tree: Option<crate::tools::bash::ChildTree>,
    /// Its input: everything goes through one writer, and dropping every
    /// sender closes stdin, which ends the process after its turn.
    wtx: Option<mpsc::UnboundedSender<String>>,
    writer: tokio::task::JoinHandle<()>,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    /// Its error output, last few KB (for a failure message).
    err_tail: tokio::task::JoinHandle<String>,
    /// Its context file, removed with it.
    scratch: Option<RunFiles>,
    /// Lines read while it was kept between turns, for the turn that takes
    /// it up (it carried on by itself, see [`Parked`]).
    backlog: std::collections::VecDeque<String>,
}

impl Proc {
    /// Start `claude` with `args` in `dir`, inside the command sandbox, and
    /// send it `init` (the `initialize` control request) first.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        path: &Path,
        account: &cc::Account,
        args: Vec<String>,
        directory: crate::tools::file_object::ProcessDirectory,
        sandbox: Option<&(crate::sandbox::Spec, bool)>,
        scratch: Option<RunFiles>,
        init: String,
        pass_env: &[String],
        private: bool,
    ) -> Result<Self, String> {
        let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        let (mut cmd, held) = claude_command(
            path, account, args, directory, sandbox, pass_env, true, private,
        )?;
        let mut child =
            crate::platform::spawn(&mut cmd, crate::platform::Start::Agent).map_err(|e| {
                format!(
                    "Could not start Claude Code ({}): {}",
                    path.display(),
                    crate::tools::file_object::launch_error(&e)
                )
            })?;
        let held = held.after_spawn();
        #[cfg(target_os = "linux")]
        let tree = child.id().map(crate::tools::bash::ChildTree::capture);
        #[cfg(windows)]
        let windows_job = child.id().and_then(crate::platform::windows::take_job);
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            #[cfg(windows)]
            if let Some(job) = windows_job {
                job.end_off_worker();
            }
            return Err("Claude Code's pipes are missing".into());
        };
        let err_tail = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 4096);
                }
            }
            String::from_utf8_lossy(&tail).into_owned()
        });
        let (wtx, mut wrx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = wrx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let _ = wtx.send(init);
        Ok(Self {
            child,
            _directory: held,
            #[cfg(windows)]
            windows_job,
            #[cfg(target_os = "linux")]
            tree,
            wtx: Some(wtx),
            writer,
            lines: BufReader::new(stdout).lines(),
            err_tail,
            scratch,
            backlog: Default::default(),
        })
    }

    /// Its next line: what it said while kept first (cancel-safe: a kept
    /// line is taken at the first poll, which then completes).
    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        if let Some(line) = self.backlog.pop_front() {
            return Ok(Some(line));
        }
        self.lines.next_line().await
    }

    /// Close its input and let it end (killed if it doesn't within a few
    /// seconds): what it wrote to stderr last, and how it ended.
    async fn close(mut self) -> Ended {
        self.wtx = None;
        self.writer.abort();
        #[cfg(target_os = "linux")]
        {
            crate::tools::bash::wait_unreaped(&self.child, std::time::Duration::from_secs(5)).await;
            if let Some(tree) = self.tree.take() {
                tree.stop_unreaped(&self.child).await;
            }
        }
        let status =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await;
        let how = match &status {
            Ok(Ok(s)) => exit_how(s),
            _ => None,
        };
        // Windows ownership survives a leader's reap, so clean descendants
        // even after polite exit. Never reacquire ownership through its pid.
        #[cfg(windows)]
        if let Some(job) = self.windows_job.take() {
            let checked = tokio::task::spawn_blocking(move || {
                job.end_checked(std::time::Duration::from_secs(3))
            })
            .await;
            if !matches!(checked, Ok(Ok(0))) {
                tracing::warn!("Claude process cleanup not checked or incomplete (Windows Job)");
            }
        }
        if !matches!(status, Ok(Ok(_))) {
            #[cfg(all(unix, not(target_os = "linux")))]
            crate::tools::bash::signal_unreaped(&mut self.child);
            let _ = self.child.start_kill();
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await;
        }
        drop(self.scratch);
        let stderr = tokio::time::timeout(std::time::Duration::from_secs(2), self.err_tail)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        Ended { stderr, how }
    }
}

/// How a `claude` process ended.
#[derive(Default)]
struct Ended {
    /// What it wrote to stderr last.
    stderr: String,
    /// Its exit code or the signal that ended it, unless it exited cleanly.
    how: Option<String>,
}

/// An exit status in plain words (`None` for a clean exit).
fn exit_how(s: &std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return Some(format!("signal {sig}"));
        }
    }
    match s.code() {
        Some(0) | None => None,
        Some(c) => Some(format!("exit code {c}")),
    }
}

/// Claude Code processes kept open between a chat's turns, and the
/// subagents the user stopped alone.
///
/// Starting `claude -p` costs a few seconds before its first token (process
/// start, sign-in, its MCP servers, loading the conversation): measured with
/// CLI 2.1.286 and Haiku 3.8 s for a new conversation and 3.0 s
/// with `--resume`, against 0.6 s for a message to a process already open;
/// through Tessera on Sonnet 5.5, 5.1 s against 1.4 s to the first token.
/// So after a turn that ended cleanly (nothing left in the background) its
/// process stays open, idle, for `agent.claude_keep_open_minutes`; the chat's
/// next turn takes it when it would start the same process now (same flags,
/// folder, sandbox, context, hooks and conversation), else it is closed and a
/// new one starts. When it takes a turn up by itself meanwhile (background
/// work Tessera didn't see ended), that becomes a turn of the chat, as a
/// message would start one ([`Inner::claude_carried_on`]): approvals,
/// Tessera's tools, Stop and the completion check work as in any turn.
/// Scheduled wake-ups keep it from being kept. At most [`MAX_PARKED`] are
/// kept, the longest idle goes first.
#[derive(Default)]
pub(super) struct ClaudeProcs {
    parked: Mutex<HashMap<String, Parked>>,
    /// `Agent` calls whose subagents the user stopped, by chat.
    stops: Mutex<HashMap<String, Vec<String>>>,
    /// Chats whose next turn continues what their kept process started by
    /// itself: there is nothing new to send it.
    resumed: Mutex<std::collections::HashSet<String>>,
}

impl ClaudeProcs {
    fn take(&self, sid: &str) -> Option<Parked> {
        self.parked.lock().remove(sid)
    }

    /// A kept process ended by itself.
    fn forget(&self, sid: &str, id: &str) {
        let mut all = self.parked.lock();
        if all.get(sid).is_some_and(|p| p.id == id) {
            all.remove(sid);
        }
    }

    /// Close the process kept for a chat (its conversation was rewound).
    pub(super) fn close_for(&self, sid: &str) {
        if let Some(p) = self.take(sid) {
            tokio::spawn(p.close());
        }
    }
}

/// How long a kept process that took a turn up by itself waits for the
/// chat's turn to take it over before it is closed.
const HANDOFF_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A process kept between turns; a task reads it meanwhile.
struct Parked {
    id: String,
    /// How it was started (see `claude_code_attempt`).
    key: String,
    /// The Claude Code conversation it holds.
    session: String,
    since: std::time::Instant,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Option<Proc>>,
    /// It took a turn up by itself; what it said so far waits in its backlog.
    carried: Arc<std::sync::atomic::AtomicBool>,
}

impl Parked {
    fn carried(&self) -> bool {
        self.carried.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The process back, still open (`None`: it ended meanwhile).
    async fn resume(self) -> Option<Proc> {
        self.cancel.cancel();
        self.task.await.ok().flatten()
    }

    async fn close(self) {
        if let Some(p) = self.resume().await {
            p.close().await;
        }
    }
}

/// What the chat said about a kept process that took a turn up by itself.
#[derive(Debug, PartialEq)]
pub(super) enum Handoff {
    /// A turn of the chat starts and takes the process.
    Started,
    /// The chat is in a turn already; it takes the process (or ends and
    /// the next try starts one).
    Busy,
    /// It can't go on (the reason, for the log); the process is closed.
    Refused(&'static str),
}

/// What a line from a kept process means between turns.
#[derive(Debug, PartialEq)]
enum ParkedLine {
    /// Nothing to do (control requests got a plain answer: nothing runs now).
    Quiet,
    /// It took a turn up by itself: the chat's turn goes on with it.
    Work,
}

fn parked_line(v: &Value, wtx: Option<&mpsc::UnboundedSender<String>>) -> ParkedLine {
    if v.get("type").and_then(Value::as_str) == Some("control_request") {
        const IDLE: &str = "This chat has no active turn, so the action did not run. Carry on with what you were doing, or wait for the user's next message.";
        if let Some(tx) = wtx {
            let _ = tx.send(cc::decline_request(v, IDLE));
        }
        return ParkedLine::Quiet;
    }
    if BackgroundWait::answers(v) {
        ParkedLine::Work
    } else {
        ParkedLine::Quiet
    }
}

/// A kept process took a turn up by itself: ask the chat to go on with it
/// until its turn takes the process (`None`) or it can't (why).
async fn hand_over(
    engine: &std::sync::Weak<Inner>,
    sid: &str,
    cancel: &CancellationToken,
) -> Option<&'static str> {
    let until = tokio::time::Instant::now() + HANDOFF_WAIT;
    // One turn is started for it at most; a turn already running takes it,
    // or it ends without and the next try starts one.
    let mut started = false;
    loop {
        // Taken back or closed meanwhile (a rewind): nothing to hand over.
        if cancel.is_cancelled() {
            return None;
        }
        if !started {
            let Some(me) = engine.upgrade() else {
                return Some("the server is stopping");
            };
            match me.claude_carried_on(sid) {
                Handoff::Started => started = true,
                Handoff::Busy => {}
                Handoff::Refused(why) => return Some(why),
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
        if tokio::time::Instant::now() >= until {
            return Some("no turn took up its work");
        }
    }
}

/// Keep the authorized file object before Claude Code changes it. Missing
/// files get `None` (rewind deletes a created file); oversized files are omitted.
/// Unsafe targets fail the hook before any bytes can enter the snapshot store.
pub(super) fn snapshot_before_edit(ctx: &ToolCtx, path: &Path) -> Result<(), String> {
    if ctx.cancel.is_cancelled() {
        return Err("The edit was cancelled; no snapshot was taken.".into());
    }
    let opened = match crate::tools::file_object::Opened::open(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A dangling link is not proof that the approved file was absent.
            if std::fs::symlink_metadata(path).is_ok() {
                return Err("The edit target changed; submit a new action.".into());
            }
            crate::tools::files::authorize_file(&crate::tools::canonical(path), ctx, false)?;
            return ctx
                .host
                .store()
                .snapshot_file(&ctx.session_id, ctx.seq, &path.to_string_lossy(), None)
                .map_err(|e| e.to_string());
        }
        Err(e) => return Err(format!("Cannot safely open the edit target: {e}")),
    };
    snapshot_opened_before_edit(ctx, path, &opened)
}

fn snapshot_opened_before_edit(
    ctx: &ToolCtx,
    path: &Path,
    opened: &crate::tools::file_object::Opened,
) -> Result<(), String> {
    use std::io::Read;

    // Same object-bound authorization as native reads. A native CLI file
    // action cannot return raw credential files, even in Auto mode.
    crate::tools::files::authorize_file(&opened.path, ctx, false)?;
    if crate::tools::credential_file(&opened.path) {
        return Err("Credential files cannot be copied by Claude Code's native edit hook.".into());
    }
    opened.check_path().map_err(|e| e.to_string())?;
    let meta = opened.file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_SNAPSHOT {
        return Ok(());
    }
    if ctx.cancel.is_cancelled() {
        return Err("The edit was cancelled; no snapshot was taken.".into());
    }
    // Metadata and bytes come from the same authorized descriptor, never a
    // reopened pathname. Limit the read too: the file may grow after metadata.
    let mut content = Vec::new();
    (&opened.file)
        .take(MAX_SNAPSHOT + 1)
        .read_to_end(&mut content)
        .map_err(|e| e.to_string())?;
    opened.check_path().map_err(|e| e.to_string())?;
    if ctx.cancel.is_cancelled() {
        return Err("The edit was cancelled; no snapshot was taken.".into());
    }
    if content.len() as u64 > MAX_SNAPSHOT {
        return Ok(());
    }
    ctx.host
        .store()
        .snapshot_file(
            &ctx.session_id,
            ctx.seq,
            &path.to_string_lossy(),
            Some(&content),
        )
        .map_err(|e| e.to_string())
}

/// Tessera's context is kept per Claude Code conversation (`Link::context`);
/// what renews the native system prompt mid-chat (settings, credentials, a
/// bot's persona, a channel's members) renews it on the next turn too.
pub(super) fn renew_claude_context(internal: &mut SessionInternal) {
    for l in internal
        .claude_code
        .iter_mut()
        .chain(internal.claude_code_bots.values_mut())
    {
        l.context = None;
    }
}

/// Where a Claude account's model list is kept between runs (the usual
/// account's under its old name).
fn claude_models_key(provider: &str) -> String {
    if provider == cc::PROVIDER {
        "claude_code_models".into()
    } else {
        format!("claude_code_models:{provider}")
    }
}

/// A secure-card-only save refreshes bridge schemas on the next turn while
/// keeping a safe warm CLI. Flags, sandbox, account, frozen context and epoch
/// must still match exactly. The key ends with schemas then context epoch.
fn same_launch_with_new_tool_schemas(a: &str, b: &str) -> bool {
    // Compare borrowed slices only; these keys contain metadata, never values.
    let (Some((a, ae)), Some((b, be))) = (a.rsplit_once('\u{1e}'), b.rsplit_once('\u{1e}')) else {
        return false;
    };
    let (Some((a, _)), Some((b, _))) = (a.rsplit_once('\u{1e}'), b.rsplit_once('\u{1e}')) else {
        return false;
    };
    a == b && ae == be
}
fn dir_key(cwd: &Path) -> String {
    cwd.display().to_string()
}

/// A bridged tool's name as Tessera shows it (`mcp__tessera__note` → `note`).
fn shown_part(p: AssistantPart) -> AssistantPart {
    match p {
        AssistantPart::ToolCall {
            id,
            name,
            mut arguments,
        } => {
            let name = cc::shown_name(&name).to_string();
            if name == "save_secret"
                && let Some(a) = arguments.as_object_mut()
            {
                a.retain(|k, _| ["action", "name", "title", "kind", "url"].contains(&k.as_str()));
            }
            AssistantPart::ToolCall {
                id,
                name,
                arguments,
            }
        }
        other => other,
    }
}

/// Files Tessera writes for one `claude` run (its context, a system prompt),
/// readable only by the user, in a folder removed when the run is over.
struct RunFiles {
    dir: PathBuf,
    context: PathBuf,
    system: PathBuf,
}

impl RunFiles {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("tessera-claude-{}", new_id()));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&dir)?;
        Ok(Self {
            context: dir.join("tessera-context.md"),
            system: dir.join("system.md"),
            dir,
        })
    }

    fn with_context(text: &str) -> std::io::Result<Self> {
        let f = Self::new()?;
        crate::config::write_atomic(&f.context, text.as_bytes(), true)
            .map_err(std::io::Error::other)?;
        Ok(f)
    }

    fn with_system(text: &str) -> std::io::Result<Self> {
        let f = Self::new()?;
        crate::config::write_atomic(&f.system, text.as_bytes(), true)
            .map_err(std::io::Error::other)?;
        Ok(f)
    }
}

impl Drop for RunFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn actor_is_read_only(req: &Value) -> bool {
    req.get("permission_mode").and_then(Value::as_str) == Some("plan")
        || matches!(
            req.get("agent_type").and_then(Value::as_str),
            Some("Explore" | "Plan")
        )
}

/// Session hand-offs aren't made safe by a read-only wrapper over Meta tools.
fn delegation_tool(name: &str, args: &Value) -> bool {
    matches!(name, "agent" | "agents" | "message_bot")
        || (name == "schedule" && crate::tools::meta::schedule_action(args) != Ok("list"))
}

fn main_actor(req: &Value) -> bool {
    req.is_object()
        && req.get("agent_id").is_none_or(Value::is_null)
        && req.get("agent_type").is_none_or(Value::is_null)
        && req.get("permission_mode").is_none_or(|v| {
            v.is_null()
                || matches!(
                    v.as_str(),
                    Some(
                        "default"
                            | "acceptEdits"
                            | "plan"
                            | "bypassPermissions"
                            | "dontAsk"
                            | "auto"
                    )
                )
        })
}

struct MainAdmission {
    tool: String,
    input: Value,
    actor: Value,
    mode: PermissionMode,
}

#[derive(Default)]
struct DelegationAdmissions {
    admitted: HashMap<String, MainAdmission>,
    // Per-turn tombstones survive tool completion and every repeated hook.
    consumed: HashSet<String>,
    in_flight: HashSet<String>,
}

impl DelegationAdmissions {
    fn retired(&self, call: &str) -> bool {
        self.consumed.contains(call) || self.in_flight.contains(call)
    }

    /// Every gate answer, including asynchronous ones, publishes through the
    /// same lock as MCP consumption. Successful enqueue precedes authority.
    fn publish(
        &mut self,
        id: String,
        admission: Option<MainAdmission>,
        enqueue: impl FnOnce(bool) -> bool,
    ) {
        self.admitted.remove(&id);
        let available = !self.retired(&id);
        if !available && admission.is_some() {
            tracing::warn!(call_id = %id, "claude code: refused delegation admission rearm");
        }
        if enqueue(available)
            && available
            && let Some(admission) = admission
        {
            self.admitted.insert(id, admission);
        }
    }
}

/// Tessera's tools in this turn (see the module docs).
struct Bridge {
    tools: Vec<ToolSpec>,
    /// What bridged calls of the main conversation returned, by call id,
    /// for their transcript entries.
    done: Arc<Mutex<HashMap<String, ToolOutput>>>,
    /// Cancelled when the turn ends: a call still running stops, and one
    /// that arrives later is refused.
    cancel: CancellationToken,
}

/// One of Claude Code's own subagents (`Agent`): its card, and the session
/// that shows what it does like one of Tessera's subagents.
#[derive(Default)]
struct AgentRun {
    kind: String,
    description: String,
    /// The request is not proof of launch; an error result ends it.
    requested_background: bool,
    background: bool,
    steps: u64,
    tool_uses: u64,
    tokens: u64,
    duration_ms: u64,
    status: String,
    /// Its session (none in private chats).
    child: Option<Arc<Rt>>,
    /// The model it runs, as its messages say.
    model: Option<String>,
    /// Its message being received.
    pending: Option<Pending>,
    /// Its tool calls without a result yet (id, name).
    open: Vec<(String, String)>,
    started: bool,
    done: bool,
    /// The user stopped it alone (`stop_task`).
    stop_asked: bool,
}

impl AgentRun {
    /// `subagent` for its card; `subagents` as Tessera's own `agent` calls
    /// have them (its session).
    fn details(&self) -> Value {
        let mut d = json!({"subagent": {
            "type": self.kind,
            "description": self.description,
            "background": self.background,
            "tool_uses": self.tool_uses.max(self.steps),
            "tokens": self.tokens,
            "duration_ms": self.duration_ms,
            "status": self.status,
        }});
        if let Some(c) = &self.child {
            let name = if self.description.is_empty() {
                "Subagent"
            } else {
                &self.description
            };
            d["subagents"] = json!([{
                "name": name,
                "session_id": c.st.lock().info.id,
                "background": self.background,
            }]);
        }
        d
    }
}

enum Policy {
    Allow,
    Deny(String),
    Ask { reason: String, key: String },
}

/// An assistant message being received (it arrives block by block).
#[derive(Default)]
struct Pending {
    id: String,
    parts: Vec<AssistantPart>,
    usage: Usage,
}

struct PermissionGate {
    tool: String,
    input: Value,
    mode: PermissionMode,
    result: tokio::sync::watch::Receiver<Option<ApprovalOutcome>>,
}

struct QuestionGate {
    input: Value,
    result: tokio::sync::watch::Receiver<Option<Result<Value, String>>>,
    token: CancellationToken,
    waiters: Arc<std::sync::atomic::AtomicUsize>,
    owner: Arc<Mutex<super::helper_limits::Provisional>>,
}
struct QuestionWaiter {
    waiters: Arc<std::sync::atomic::AtomicUsize>,
    token: CancellationToken,
    owner: Arc<Mutex<super::helper_limits::Provisional>>,
}
impl Drop for QuestionWaiter {
    fn drop(&mut self) {
        if self
            .waiters
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.owner.lock().cancel_unreleased();
            self.token.cancel();
        }
    }
}

struct Turn {
    sid: String,
    model: ModelRef,
    cwd: PathBuf,
    mode: PermissionMode,
    author: Option<String>,
    command: String,
    /// No earlier Claude Code conversation for this chat.
    first: bool,
    /// A channel: queued messages start a new round instead.
    room: bool,
    /// Its conversation started (`init`).
    started: bool,
    /// It compacted its conversation during this run.
    compacted: bool,
    context_request: Option<String>,
    context_last: Option<std::time::Instant>,
    context_supported: bool,
    context_reported: bool,
    claude_session: Option<String>,
    /// The concrete model Claude Code runs (e.g. `claude-sonnet-5-5`).
    resolved: Option<String>,
    /// Its usage totals so far in this conversation.
    reported: std::collections::BTreeMap<String, cc::Reported>,
    pending: Option<Pending>,
    /// Messages whose usage is already on an entry.
    counted: Vec<String>,
    /// The message streaming now and its tool blocks by index.
    current: Option<String>,
    tool_blocks: HashMap<u32, (String, String)>,
    streaming: bool,
    /// Streamed pieces waiting to go out together (see `deltas.rs`).
    deltas: super::deltas::Batcher,
    /// Tool calls without a result yet.
    open: Vec<(String, String)>,
    /// The transcript entry each tool call is in.
    calls: HashMap<String, u64>,
    /// Permission prompts waiting for the user, by request id.
    approvals: HashMap<String, CancellationToken>,
    gates: HashMap<String, PermissionGate>,
    questions: HashMap<String, QuestionGate>,
    policy_hooks: HashMap<String, Value>,
    admissions: Arc<Mutex<DelegationAdmissions>>,
    cancel: CancellationToken,
    /// Tessera's tools, when it has them.
    bridge: Option<Bridge>,
    /// Its subagents, by the `Agent` call that started them.
    agents: HashMap<String, AgentRun>,
    /// Tasks it runs (task id → its call, and whether in the background).
    tasks: HashMap<String, (String, bool)>,
    /// Terminal task IDs, call IDs and aliases for this Turn's entire life.
    terminal_background: HashSet<String>,
    /// Messages sent to it, and those it took up (echoed back): one it
    /// hasn't taken up yet still gets a turn of its own.
    sent: u32,
    taken: u32,
    /// The last accepted input or output before any uncertain delivery.
    last: Option<(u64, u64)>,
    finished: bool,
    failure: Option<String>,
    limit_noticed: bool,
    /// The public rejection, including reset-less and credits-required hits.
    limit: Option<limits::Hit>,
    /// Fast mode was asked for (and whether its being off was said).
    fast: bool,
    fast_noticed: bool,
    guard: Guardrails,
    post_checks: Arc<Mutex<HashMap<String, (Value, ToolOutput)>>>,
    guard_notes: Vec<String>,
    turn_from: u64,
    /// Last ledger received, not merely queued to the stdin writer.
    ledger_version: String,
    pending_inputs: std::collections::VecDeque<PendingInput>,
    input_uncertain: bool,
    /// Raw arguments for scoped observation (also for CLI children).
    call_args: HashMap<String, Value>,
    /// Copies are permitted only after the policy hook authorized the edit.
    snapshots: bool,
    /// Edit calls Tessera kept a copy for: the file and the entry it belongs to.
    edits: HashMap<String, (PathBuf, u64)>,
    /// The file each edit call names (from the call itself).
    edit_paths: HashMap<String, PathBuf>,
    /// Entries whose copies get what the files hold at the end of the run.
    snapped: Vec<u64>,
    /// Calls Tessera or the user refused.
    denied: Arc<Mutex<HashMap<String, ApprovalOutcome>>>,
    /// The verification note was given in this turn.
    nudged: bool,
    /// Scheduled wake-ups reported by the latest Stop hook snapshot.
    crons: usize,
    /// Something of its own reached the transcript since its last result.
    said: bool,
    /// Its last message was an error of the API's (shown if the result isn't).
    api_error: Option<String>,
    /// Its retries were told about in this turn.
    retry_noticed: bool,
    /// Messages taken up (`taken`) as of its last result.
    answered: u32,
    /// This run took up a turn its kept process started by itself: what it
    /// says moves the chat's resume point on, though nothing was sent to it.
    adopted: bool,
    /// Background work it waited for ended after its result: it takes the
    /// turn up again by itself (until its next result).
    carry_on_expected: bool,
}

/// A successful enqueue is only pending: the writer can still lose stdin.
struct PendingInput {
    uuid: String,
    fingerprint: String,
    ledger_version: Option<String>,
    boundary: Option<(u64, u64)>,
    /// An admitted own native helper changed the outstanding dependencies.
    completion_dependency: bool,
}

fn input_fingerprint(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    (message.get("role").and_then(Value::as_str) == Some("user"))
        .then(|| crate::util::sha256_hex(content.to_string().as_bytes()))
}

impl Turn {
    /// Task IDs (including result-only identities) and Agent calls are two
    /// views of the same work. Multiple IDs for one call count only once.
    fn background_count(&self) -> usize {
        let calls: HashSet<&str> = self
            .tasks
            .values()
            .filter_map(|(c, _)| (!c.is_empty()).then_some(c.as_str()))
            .chain(
                self.agents
                    .iter()
                    .filter(|(_, a)| a.background && !a.done)
                    .map(|(c, _)| c.as_str()),
            )
            .collect();
        self.crons + calls.len() + self.tasks.values().filter(|(c, _)| c.is_empty()).count()
    }

    /// A missing launch result is not proof that a requested Agent never ran.
    /// Keep input open, without inventing a confirmed-running count.
    fn retain_background(&self) -> bool {
        self.background_count() > 0
            || self
                .agents
                .values()
                .any(|a| a.requested_background && !a.done)
    }

    fn background_ended(&self, task: Option<&str>, call: Option<&str>) -> bool {
        [task, call]
            .into_iter()
            .flatten()
            .any(|id| self.terminal_background.contains(id))
    }

    fn link_background(&mut self, task: String, call: String, background: bool) {
        if task.is_empty() {
            return;
        }
        if self.background_ended(Some(&task), Some(&call)) {
            self.finish_background(Some(&task), Some(&call));
            return;
        }
        let entry = self
            .tasks
            .entry(task)
            .or_insert_with(|| (call.clone(), background));
        if entry.0.is_empty() {
            entry.0 = call;
        }
        entry.1 |= background; // A missing flag never erases known launch evidence.
    }

    /// End all aliases together; replay cannot renew completion/grace state.
    fn finish_background(&mut self, task: Option<&str>, call: Option<&str>) -> bool {
        let call = task
            .and_then(|id| self.tasks.get(id))
            .map(|(c, _)| c.as_str())
            .filter(|c| !c.is_empty())
            .or(call)
            .unwrap_or_default()
            .to_string();
        let ids: Vec<String> = self
            .tasks
            .iter()
            .filter(|(id, (c, _))| task == Some(id.as_str()) || (!call.is_empty() && *c == call))
            .map(|(id, _)| id.clone())
            .collect();
        let changed = !ids.is_empty()
            || self
                .agents
                .get(&call)
                .is_some_and(|a| !a.done && (a.background || a.requested_background));
        for id in ids {
            self.tasks.remove(&id);
            self.terminal_background.insert(id);
        }
        if let Some(task) = task.filter(|id| !id.is_empty()) {
            self.terminal_background.insert(task.to_string());
        }
        if !call.is_empty() {
            self.terminal_background.insert(call.clone());
        }
        if let Some(a) = self.agents.get_mut(&call) {
            a.background = false;
            a.requested_background = false;
        }
        self.carry_on_expected |= changed && self.finished;
        changed
    }

    fn background_result(&mut self, call: &str, name: &str, v: &Value, failed: bool) {
        let result = v.get("tool_use_result").unwrap_or(&Value::Null);
        let task = (cc::is_agent(name) || matches!(name, "Bash" | "PowerShell" | "Monitor"))
            .then(|| result.get("backgroundTaskId").and_then(Value::as_str))
            .flatten()
            .filter(|id| !id.is_empty());
        let agent = cc::is_agent(name)
            .then(|| result.get("agentId").and_then(Value::as_str))
            .flatten()
            .filter(|id| !id.is_empty());
        let ids = [task, agent];
        // Check every new alias BEFORE promotion. A task-only completion can
        // precede both its call association and a distinct agentId alias.
        if failed
            || self.background_ended(None, Some(call))
            || ids
                .into_iter()
                .flatten()
                .any(|id| self.terminal_background.contains(id))
        {
            self.finish_background(None, Some(call));
            self.terminal_background
                .extend(ids.into_iter().flatten().map(str::to_string));
            return;
        }
        let background = task.is_some()
            || self.tasks.values().any(|(c, bg)| c == call && *bg)
            || self
                .agents
                .get(call)
                .is_some_and(|a| a.background || a.requested_background);
        if background {
            for id in ids.into_iter().flatten() {
                self.link_background(id.to_string(), call.to_string(), true);
            }
            if let Some(a) = self.agents.get_mut(call) {
                a.background = true;
            }
        } else {
            // agentId alone is correlation metadata, not launch evidence.
            self.finish_background(None, Some(call));
            self.terminal_background
                .extend(ids.into_iter().flatten().map(str::to_string));
        }
    }

    /// claude_agent_end has saved the child; keep it only until its own
    /// outstanding tool result has acquired the card's details.
    fn retire_agents(&mut self) {
        self.agents
            .retain(|call, a| !a.done || self.open.iter().any(|(id, _)| id == call));
        let terminal = &self.terminal_background;
        let open = &self.open;
        let resolved =
            |call: &str| terminal.contains(call) && !open.iter().any(|(id, _)| id == call);
        self.call_args.retain(|call, _| !resolved(call));
        self.calls.retain(|call, _| !resolved(call));
    }

    fn inputs_confirmed(&self) -> bool {
        self.pending_inputs.is_empty() && !self.input_uncertain
    }

    fn delivered_ledger_version(&self) -> String {
        if self.inputs_confirmed() {
            self.ledger_version.clone()
        } else {
            String::new()
        }
    }

    fn record_last(&mut self, boundary: (u64, u64)) {
        if (self.taken > 0 || self.adopted)
            && self.inputs_confirmed()
            && self.last.is_none_or(|last| last.0 < boundary.0)
        {
            self.last = Some(boundary);
        }
    }

    fn send_input(
        &mut self,
        tx: &mpsc::UnboundedSender<String>,
        content: &[ContentPart],
        ledger_version: Option<String>,
        boundary: Option<(u64, u64)>,
    ) -> bool {
        // SDK streaming inputs accept a caller-supplied UUID, replayed on
        // acceptance. It stays in transport metadata, not the model prompt.
        let nonce = crate::util::sha256_hex(new_id().as_bytes());
        let uuid = format!(
            "{}-{}-4{}-a{}-{}",
            &nonce[..8],
            &nonce[8..12],
            &nonce[13..16],
            &nonce[17..20],
            &nonce[20..32]
        );
        let mut message: Value =
            serde_json::from_str(&cc::user_line(content)).expect("user line serializes");
        message["uuid"] = json!(uuid);
        let fingerprint = input_fingerprint(&message["message"]).expect("user input has content");
        if tx.send(message.to_string()).is_err() {
            self.input_uncertain = true;
            self.failure = Some(
                "Claude Code's input closed before it received the message; continue to resend it."
                    .into(),
            );
            return false;
        }
        self.pending_inputs.push_back(PendingInput {
            uuid,
            fingerprint,
            ledger_version,
            boundary,
            completion_dependency: false,
        });
        self.sent += 1;
        true
    }

    /// Returns whether an own dependency report was confirmed this time.
    fn accept_input(&mut self, replay: &Value) -> bool {
        // Exact UUID + content/FIFO correlation, not a replay count. Context
        // replays, duplicate/foreign echoes and CLI children prove nothing.
        if replay.get("isReplay").and_then(Value::as_bool) != Some(true)
            || replay.get("isSynthetic").and_then(Value::as_bool) == Some(true)
            || replay
                .get("parent_tool_use_id")
                .is_some_and(|p| !p.is_null())
        {
            return false;
        }
        let Some(fingerprint) = input_fingerprint(&replay["message"]) else {
            return false;
        };
        if self.pending_inputs.front().is_none_or(|p| {
            replay.get("uuid").and_then(Value::as_str) != Some(p.uuid.as_str())
                || p.fingerprint != fingerprint
        }) {
            return false;
        }
        let input = self
            .pending_inputs
            .pop_front()
            .expect("matched pending input");
        self.taken += 1;
        if let Some(version) = input.ledger_version {
            self.ledger_version = version;
        }
        // Acceptance confirms this boundary even while later inputs await
        // their own echo; subsequent output must not skip those inputs.
        if let Some(boundary) = input.boundary
            && self.last.is_none_or(|last| last.0 < boundary.0)
        {
            self.last = Some(boundary);
        }
        if input.completion_dependency {
            self.nudged = false;
        }
        input.completion_dependency
    }
    fn new(
        sid: String,
        model: ModelRef,
        cwd: PathBuf,
        mode: PermissionMode,
        author: Option<String>,
    ) -> Self {
        Self {
            sid,
            model,
            cwd: cwd.clone(),
            mode,
            author,
            command: String::new(),
            first: false,
            room: false,
            started: false,
            compacted: false,
            context_request: None,
            context_last: None,
            context_supported: true,
            context_reported: false,
            claude_session: None,
            resolved: None,
            reported: Default::default(),
            pending: None,
            counted: Vec::new(),
            current: None,
            tool_blocks: HashMap::new(),
            streaming: false,
            deltas: Default::default(),
            open: Vec::new(),
            calls: HashMap::new(),
            approvals: HashMap::new(),
            gates: HashMap::new(),
            questions: HashMap::new(),
            policy_hooks: HashMap::new(),
            admissions: Arc::new(Mutex::new(DelegationAdmissions::default())),
            cancel: CancellationToken::new(),
            bridge: None,
            agents: HashMap::new(),
            tasks: HashMap::new(),
            terminal_background: HashSet::new(),
            sent: 0,
            taken: 0,
            last: None,
            finished: false,
            failure: None,
            limit_noticed: false,
            limit: None,
            fast: false,
            fast_noticed: false,
            guard: Guardrails::new(
                LoopLimits::default(),
                verify::Verification::new(VerifyCheck::Off, cwd.clone()),
            ),
            post_checks: Default::default(),
            guard_notes: Vec::new(),
            turn_from: 0,
            ledger_version: String::new(),
            pending_inputs: Default::default(),
            input_uncertain: false,
            call_args: HashMap::new(),
            snapshots: false,
            edits: HashMap::new(),
            edit_paths: HashMap::new(),
            snapped: Vec::new(),
            denied: Default::default(),
            nudged: false,
            crons: 0,
            said: false,
            api_error: None,
            retry_noticed: false,
            answered: 0,
            adopted: false,
            carry_on_expected: false,
        }
    }

    /// One of its tool calls (or a subagent's): a command that checks the
    /// code, or the file an edit changes.
    fn note_check(&mut self, id: &str, name: &str, args: &Value) {
        self.call_args
            .entry(id.to_string())
            .or_insert_with(|| args.clone());
        if cc::is_edit_tool(name)
            && let Some(p) = cc::edit_path(args)
        {
            self.edit_paths.insert(id.to_string(), self.cwd.join(p));
        }
    }
}

/// Stable content version: a reset/clear is an update too. The full ledger
/// lives outside Claude's transcript and never changes its frozen system prompt.
fn ledger_context(ledger: Option<&TaskLedger>) -> (String, String) {
    let ledger = ledger.cloned().unwrap_or_default();
    let version = crate::tools::meta::ledger_version(Some(&ledger));
    let text = format!(
        "[Tessera work state, version {version}]\nThis replaces previous work state, including any cleared goal. Use Tessera's `task` for updates and `recall` for exact earlier evidence.\n{}",
        crate::tools::meta::render_ledger(&ledger)
    );
    (version, text)
}

/// What Claude Code is sent: the new message(s) at the end of the
/// transcript, after the part of the chat it hasn't seen (another model's
/// turns, or everything when its conversation can't be continued). A
/// message someone else wrote starts with `[label]:` (`label` by author;
/// empty: none).
fn prompt(entries: &[Entry], seen: u64, label: &dyn Fn(&str) -> String) -> Vec<ContentPart> {
    let tail = entries
        .iter()
        .rposition(|e| !matches!(e.body, EntryBody::User { .. } | EntryBody::Notice { .. }))
        .map_or(0, |i| i + 1);
    let mut out = transfer(
        &entries[..tail]
            .iter()
            .filter(|e| e.seq > seen)
            .collect::<Vec<_>>(),
        label,
    );
    if !out.is_empty() {
        out.push(ContentPart::text("[The new message:]"));
    }
    for e in &entries[tail..] {
        if let EntryBody::User { content, author } = &e.body {
            match author.as_deref().map(label).filter(|l| !l.is_empty()) {
                Some(l) => {
                    out.push(ContentPart::text(format!(
                        "[{l}]: {}",
                        tessera_proto::plain_text(content)
                    )));
                    out.extend(
                        content
                            .iter()
                            .filter(|p| matches!(p, ContentPart::Image { .. }))
                            .cloned(),
                    );
                }
                None => out.extend(content.iter().cloned()),
            }
        }
    }
    if out.is_empty() {
        out.push(ContentPart::text("Continue."));
    }
    out
}

/// What a bot on Claude Code is sent in a channel: what the user and the
/// other members said since its last reply (images as they are), then its
/// turn.
fn room_prompt(
    entries: &[Entry],
    seen: u64,
    me: &str,
    handle: &dyn Fn(&str) -> String,
) -> Vec<ContentPart> {
    let mut lines = Vec::new();
    let mut images = Vec::new();
    let recent = if seen == 0 {
        entries
            .iter()
            .rev()
            .filter(|e| matches!(e.body, EntryBody::User { .. }))
            .nth(5)
            .map_or(0, |e| e.seq)
    } else {
        seen + 1
    };
    if seen == 0 {
        if let Some((seq, summary, _)) = crate::context::last_compaction(entries) {
            lines.push(format!("[Stored checkpoint #{seq}, verbatim]\n{summary}"));
        }
        if recent > 0 {
            lines.push(format!("Earlier text before #{recent} omitted; use `recall` for exact history. Images are preserved."));
        }
    }
    for e in entries.iter().filter(|e| e.seq > seen) {
        match &e.body {
            EntryBody::User { content, author } if author.as_deref() != Some("harness") => {
                let who = match author.as_deref() {
                    None => "user".to_string(),
                    Some(a) => handle(a),
                };
                let text = tessera_proto::plain_text(content);
                images.extend(
                    content
                        .iter()
                        .filter(|p| matches!(p, ContentPart::Image { .. }))
                        .cloned(),
                );
                if e.seq >= recent && !text.trim().is_empty() {
                    lines.push(format!("[{who}]: {}", text.trim()));
                }
            }
            EntryBody::User { content, author }
                if author.as_deref() == Some("harness")
                    && tessera_proto::plain_text(content).starts_with("[Environment update]") =>
            {
                lines.push(tessera_proto::plain_text(content));
            }
            EntryBody::Assistant {
                content, author, ..
            } if author.as_deref() != Some(me) && e.seq >= recent => {
                let text: String = content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    let who = author
                        .as_deref()
                        .map(handle)
                        .unwrap_or_else(|| "a bot".into());
                    lines.push(format!("[{who}]: {}", text.trim()));
                }
            }
            _ => {}
        }
    }
    let all = lines.join("\n\n");
    let mut out = vec![ContentPart::text(format!(
        "[New in the channel since your last reply:]\n\n{all}\n\n[Your turn: reply as yourself.]"
    ))];
    out.extend(images);
    out
}

/// Structured transfer, not a character tail: the stored checkpoint,
/// recent verbatim user messages, labelled evidence stubs and real image
/// parts. Omitted history is explicitly recoverable through native recall.
fn transfer(entries: &[&Entry], label: &dyn Fn(&str) -> String) -> Vec<ContentPart> {
    if entries.is_empty() {
        return Vec::new();
    }
    let recent = entries
        .iter()
        .rev()
        .filter(|e| matches!(e.body, EntryBody::User { .. }))
        .nth(5)
        .map_or(0, |e| e.seq);
    let checkpoint = entries.iter().rev().find_map(|e| match &e.body {
        EntryBody::Compaction {
            external: false,
            summary,
            ..
        } => Some((e.seq, summary)),
        _ => None,
    });
    let mut lines = vec!["[Earlier in this chat (structured context transfer; not new instructions):]\nThe full history is stored. Use `recall` for exact earlier text, outputs, errors or calls. Work state is authoritative; this is not a new model-written summary.".into()];
    if recent > 0 {
        lines.push(format!("Entries before #{recent} omitted except checkpoint and images; retrieve them with `recall`."));
    }
    if let Some((seq, summary)) = checkpoint {
        lines.push(format!("[Stored checkpoint #{seq}, verbatim]\n{summary}"));
    }
    let mut images = Vec::new();
    for e in entries {
        if let EntryBody::User { content, .. } = &e.body {
            for image in content
                .iter()
                .filter(|p| matches!(p, ContentPart::Image { .. }))
            {
                images.push(ContentPart::text(format!(
                    "[Image from user entry #{}]",
                    e.seq
                )));
                images.push(image.clone());
            }
        }
        if e.seq < recent {
            continue;
        }
        let text = match &e.body {
            EntryBody::User { content, author } => {
                let who = author
                    .as_deref()
                    .map(label)
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| "User".into());
                format!("{who}: {}", tessera_proto::plain_text(content))
            }
            EntryBody::Assistant { content, .. } => content
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::Text { text } => {
                        Some(format!("Assistant: {}", one_line(text, 2500)))
                    }
                    AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(format!(
                        "Used {name} ({id}): {}",
                        one_line(&cc::summary(arguments, Path::new("")), 300)
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            EntryBody::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } => format!(
                "{} ({call_id}): {}",
                if *is_error { "Failed" } else { "Result" },
                one_line(&tessera_proto::plain_text(content), 400)
            ),
            _ => continue,
        };
        lines.push(format!("[Entry #{}]\n{text}", e.seq));
    }
    let mut out = vec![ContentPart::text(lines.join("\n\n"))];
    out.extend(images);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn delivery_turn() -> Turn {
        let mut t = Turn::new(
            "fixture".into(),
            "claude-code/opus".parse().unwrap(),
            PathBuf::from("."),
            PermissionMode::Auto,
            None,
        );
        t.ledger_version = "V1".into();
        t.last = Some((10, 100));
        t
    }

    fn replay(line: &str) -> Value {
        let mut v: Value = serde_json::from_str(line).unwrap();
        v["isReplay"] = json!(true);
        v
    }

    #[tokio::test]
    async fn bounded_shared_question_release_requires_one_successful_queue() {
        for all_failed in [true, false] {
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
            engine
                .0
                .reserve_helper_step(
                    &rt,
                    "AskUserQuestion",
                    &json!({"question":"one?"}),
                    Some("shared"),
                    false,
                )
                .unwrap();
            let owner = Arc::new(Mutex::new(super::super::helper_limits::Provisional::new(
                &engine.0, &rt, "shared",
            )));
            let waiters = Arc::new(std::sync::atomic::AtomicUsize::new(2));
            let token = CancellationToken::new();
            let first = QuestionWaiter {
                waiters: waiters.clone(),
                token: token.clone(),
                owner: owner.clone(),
            };
            let second = QuestionWaiter {
                waiters,
                token: token.clone(),
                owner: owner.clone(),
            };
            // The answer is ready, but this callback's receiver is gone.
            let (dead_tx, dead_rx) = mpsc::unbounded_channel::<String>();
            drop(dead_rx);
            assert!(
                !owner
                    .lock()
                    .release_to(|| dead_tx.send("allow".into()).is_ok())
                    .unwrap()
            );
            drop(first);
            assert_eq!(rt.st.lock().info.steps, 0);
            assert_eq!(rt.st.lock().budget.as_ref().unwrap().used, 1);
            assert!(!token.is_cancelled(), "one failed duplicate is local");
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            if all_failed {
                rx.close();
            }
            assert_eq!(
                owner
                    .lock()
                    .release_to(|| tx.send("allow".into()).is_ok())
                    .unwrap(),
                !all_failed
            );
            drop(second);
            let st = rt.st.lock();
            assert_eq!(st.info.steps, if all_failed { 0 } else { 1 });
            let b = st.budget.as_ref().unwrap();
            assert_eq!(b.used, if all_failed { 0 } else { 1 });
            if all_failed {
                assert!(
                    b.calls.is_empty(),
                    "all failed callbacks refund the never-released claim"
                );
            } else {
                assert!(b.calls["shared"].dispatched);
                assert_eq!(rx.try_recv().unwrap(), "allow");
            }
        }
    }
    #[tokio::test]
    async fn earlier_hook_release_survives_failed_duplicate_affirmative_send() {
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
        let args = json!({"pattern":"*"});
        engine
            .0
            .reserve_helper_step(&rt, "Glob", &args, Some("same"), false)
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut hook = super::super::helper_limits::Provisional::new(&engine.0, &rt, "same");
        assert!(
            hook.release_to(|| tx.send("hook allow".into()).is_ok())
                .unwrap()
        );
        assert_eq!(rx.try_recv().unwrap(), "hook allow");
        drop(rx);
        engine
            .0
            .reserve_helper_step(&rt, "Glob", &args, Some("same"), false)
            .unwrap();
        let mut duplicate = super::super::helper_limits::Provisional::new(&engine.0, &rt, "same");
        assert!(
            !duplicate
                .release_to(|| tx.send("permission allow".into()).is_ok())
                .unwrap()
        );
        drop(duplicate);
        let st = rt.st.lock();
        assert_eq!(st.info.steps, 1);
        let b = st.budget.as_ref().unwrap();
        assert_eq!(b.used, 1);
        assert!(b.calls["same"].dispatched && b.calls["same"].pending);
    }

    #[tokio::test]
    async fn mixed_question_cancel_and_failed_send_preserve_only_prior_release() {
        for released in [false, true] {
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
            engine
                .0
                .reserve_helper_step(
                    &rt,
                    "AskUserQuestion",
                    &json!({"question":"one?"}),
                    Some("mixed"),
                    false,
                )
                .unwrap();
            let owner = Arc::new(Mutex::new(super::super::helper_limits::Provisional::new(
                &engine.0, &rt, "mixed",
            )));
            let waiters = Arc::new(std::sync::atomic::AtomicUsize::new(2));
            let token = CancellationToken::new();
            let cancelled = QuestionWaiter {
                waiters: waiters.clone(),
                token: token.clone(),
                owner: owner.clone(),
            };
            let failed = QuestionWaiter {
                waiters,
                token: token.clone(),
                owner: owner.clone(),
            };
            if released {
                assert!(owner.lock().release_to(|| true).unwrap());
            }
            drop(cancelled); // one callback was cancelled before an answer send
            assert!(!token.is_cancelled());
            let (tx, rx) = mpsc::unbounded_channel::<String>();
            drop(rx);
            assert!(
                !owner
                    .lock()
                    .release_to(|| tx.send("allow".into()).is_ok())
                    .unwrap()
            );
            drop(failed);
            assert!(token.is_cancelled());
            let st = rt.st.lock();
            assert_eq!(st.info.steps, u32::from(released));
            let b = st.budget.as_ref().unwrap();
            assert_eq!(b.used, u32::from(released));
            if released {
                assert!(b.calls["mixed"].dispatched);
            } else {
                assert!(b.calls.is_empty());
            }
        }
    }
    #[tokio::test]
    async fn native_report_renewal_requires_one_correlated_main_acceptance() {
        let mut t = delivery_turn();
        t.nudged = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        assert!(t.send_input(
            &tx,
            &[ContentPart::text("own native report")],
            Some("V2".into()),
            Some((20, 200))
        ));
        t.pending_inputs.back_mut().unwrap().completion_dependency = true;
        let accepted = replay(&rx.recv().await.unwrap());
        let mut foreign = accepted.clone();
        foreign["uuid"] = json!("foreign");
        assert!(!t.accept_input(&foreign));
        assert!(t.nudged && !t.inputs_confirmed());
        assert_eq!((t.ledger_version.as_str(), t.last), ("V1", Some((10, 100))));
        assert!(t.accept_input(&accepted));
        assert!(!t.nudged && t.inputs_confirmed());
        assert_eq!((t.ledger_version.as_str(), t.last), ("V2", Some((20, 200))));
        t.nudged = true;
        assert!(!t.accept_input(&accepted));
        assert!(t.nudged, "duplicate cannot renew a consumed Stop allowance");
    }
    #[test]
    fn output_before_initial_send_cannot_skip_unsent_work() {
        let mut t = delivery_turn();
        t.record_last((20, 200));
        assert_eq!(t.last, Some((10, 100)));
        assert_eq!(t.ledger_version, "V1");
    }
    #[test]
    fn failed_ledger_send_advances_neither_version_nor_resume_boundary() {
        let mut t = delivery_turn();
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        assert!(!t.send_input(
            &tx,
            &[ContentPart::text("V2 adds a constraint")],
            Some("V2".into()),
            Some((20, 200))
        ));
        t.record_last((21, 210));
        assert_eq!(t.ledger_version, "V1");
        assert_eq!(t.last, Some((10, 100)));
        assert_eq!((t.sent, t.taken), (0, 0));
        assert!(t.pending_inputs.is_empty());
        assert!(
            t.delivered_ledger_version().is_empty(),
            "uncertain delivery must resend fully"
        );
    }

    #[tokio::test]
    async fn closed_channel_before_take_keeps_delivery_pending_on_restart() {
        let mut t = delivery_turn();
        let (tx, rx) = mpsc::unbounded_channel();
        assert!(t.send_input(
            &tx,
            &[ContentPart::text("V2 adds a constraint")],
            Some("V2".into()),
            Some((20, 200))
        ));
        drop(rx); // enqueue succeeded, but neither stdin nor CLI acceptance did.
        t.record_last((21, 210));
        assert_eq!(t.ledger_version, "V1");
        assert_eq!(t.last, Some((10, 100)));
        let persisted = Link {
            seq: t.last.unwrap().0,
            ts: t.last.unwrap().1,
            ledger_version: t.delivered_ledger_version(),
            ..Default::default()
        };
        let resumed: Link =
            serde_json::from_str(&serde_json::to_string(&persisted).unwrap()).unwrap();
        assert!(resumed.ledger_version.is_empty());
        assert_eq!(resumed.seq, 10);
        assert_ne!(resumed.ledger_version, "V2");
    }

    #[tokio::test]
    async fn only_correlated_acceptance_promotes_ledger_and_boundary() {
        let mut t = delivery_turn();
        let (tx, mut rx) = mpsc::unbounded_channel();
        assert!(t.send_input(
            &tx,
            &[ContentPart::text("V2")],
            Some("V2".into()),
            Some((20, 200))
        ));
        let a = replay(&rx.recv().await.unwrap());
        assert!(t.send_input(
            &tx,
            &[ContentPart::text("V3")],
            Some("V3".into()),
            Some((30, 300))
        ));
        let b = replay(&rx.recv().await.unwrap());
        for invalid in [
            b.clone(),
            {
                let mut v = a.clone();
                v["isSynthetic"] = json!(true);
                v
            },
            {
                let mut v = a.clone();
                v["parent_tool_use_id"] = json!("child");
                v
            },
            {
                let mut v = a.clone();
                v["message"]["content"][0]["text"] = json!("foreign");
                v
            },
            {
                let mut v = a.clone();
                v.as_object_mut().unwrap().remove("uuid");
                v
            },
            {
                let mut v = a.clone();
                v["uuid"] = json!("foreign");
                v
            },
        ] {
            t.accept_input(&invalid);
        }
        assert_eq!((t.sent, t.taken), (2, 0));
        assert_eq!(t.ledger_version, "V1");
        t.accept_input(&a);
        t.accept_input(&a); // duplicate must not consume V3.
        t.record_last((25, 250));
        assert_eq!((t.sent, t.taken), (2, 1));
        assert_eq!(t.last, Some((20, 200)));
        assert_eq!(t.ledger_version, "V2");
        assert!(t.delivered_ledger_version().is_empty());
        t.accept_input(&b);
        assert_eq!(t.ledger_version, "V3");
        assert_eq!(t.last, Some((30, 300)));
        assert_eq!((t.sent, t.taken), (2, 2));
        assert_eq!(t.delivered_ledger_version(), "V3");
        t.record_last((31, 310));
        assert_eq!(t.last, Some((31, 310)));
    }

    #[tokio::test]
    async fn duplicate_acceptance_cannot_confirm_an_identical_second_message() {
        let mut t = delivery_turn();
        let (tx, mut rx) = mpsc::unbounded_channel();
        for i in [20, 30] {
            assert!(t.send_input(
                &tx,
                &[ContentPart::text("same message")],
                Some("V2".into()),
                Some((i, i * 10))
            ));
        }
        let first = replay(&rx.recv().await.unwrap());
        let second = replay(&rx.recv().await.unwrap());
        assert_eq!(first["message"], second["message"]);
        assert_ne!(first["uuid"], second["uuid"]);
        t.accept_input(&first);
        t.accept_input(&first);
        assert_eq!(t.taken, 1);
        assert_eq!(t.last, Some((20, 200)));
        assert!(t.delivered_ledger_version().is_empty());
        t.accept_input(&second);
        assert_eq!(t.taken, 2);
        assert_eq!(t.last, Some((30, 300)));
        assert_eq!(t.delivered_ledger_version(), "V2");
    }

    #[test]
    fn old_unconfirmed_link_versions_are_not_trusted() {
        let old: Link = serde_json::from_value(
            json!({"session":"cli", "owner":"fixture", "seq":10, "ts":100, "ledger_version":"V2"}),
        )
        .unwrap();
        assert!(old.ledger_version.is_empty());
        let confirmed = Link {
            ledger_version: "V1".into(),
            ..old
        };
        let json = serde_json::to_value(&confirmed).unwrap();
        assert_eq!(json["confirmed_ledger_version"], "V1");
        assert!(json.get("ledger_version").is_none());
        assert_eq!(
            serde_json::from_value::<Link>(json).unwrap().ledger_version,
            "V1"
        );
    }
    fn snapshot_ctx(engine: &Engine, rt: &Rt, cwd: &Path, seq: u64) -> ToolCtx {
        let (session_id, admission) = {
            let st = rt.st.lock();
            (st.info.id.clone(), engine.0.action_admission(&st))
        };
        ToolCtx {
            session_id,
            call_id: "snapshot-fixture".into(),
            admission,
            seq,
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(engine.0.clone(), false)),
            progress: Arc::new(|_| {}),
            depth: 0,
            cleared: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hook_check_authorizes_the_opened_object_before_reading_swapped_bytes() {
        use std::io::Seek;
        use std::os::unix::fs::symlink;
        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        let ctx = snapshot_ctx(&engine, &rt, project.path(), 40);
        let protected = home.path().join("protected.txt");
        std::fs::write(&protected, "protected post-edit sentinel").unwrap();
        let path = project.path().join("edited.txt");
        std::fs::write(&path, "ordinary after edit").unwrap();
        let input = json!({"file_path": path, "new_string": "edit"});
        assert!(matches!(
            engine
                .0
                .claude_policy(&rt, &ctx.cwd, "Edit", &input, &json!({})),
            Policy::Allow
        ));
        std::fs::remove_file(&path).unwrap();
        symlink(&protected, &path).unwrap();
        let mut opened = crate::tools::file_object::Opened::open(&path).unwrap();
        assert!(crate::tools::files::authorized_opened_bytes(&opened, &ctx, MAX_SNAPSHOT).is_err());
        assert_eq!(opened.file.stream_position().unwrap(), 0);
        assert!(
            crate::tools::files::authorized_bytes(&path, &ctx, MAX_SNAPSHOT)
                .await
                .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "ordinary after edit").unwrap();
        assert_eq!(
            crate::tools::files::authorized_bytes(&path, &ctx, MAX_SNAPSHOT)
                .await
                .unwrap(),
            b"ordinary after edit"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_read_bridges_refuse_protected_symlinks_without_leaking_or_mutating() {
        use std::os::unix::fs::symlink;
        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let ctx = snapshot_ctx(&engine, &rt, project.path(), 50);
        let protected = home.path().join("protected.txt");
        std::fs::write(&protected, "protected bridge sentinel").unwrap();
        let path = project.path().join("selected.txt");
        symlink(&protected, &path).unwrap();
        let registry = crate::tools::Registry::builtin();
        for (name, args) in [
            ("read", json!({"path":path})),
            ("whiteboard", json!({"action":"show","image_path":path})),
            ("note", json!({"action":"image","path":path})),
            (
                "edit",
                json!({"patch":format!("*** Begin Patch\n*** Update File: {}\n@@\n-protected bridge sentinel\n+changed\n*** End Patch",path.display())}),
            ),
        ] {
            let out = registry.get(name).unwrap().run(args, &ctx).await;
            assert!(
                out.is_error,
                "{name} unexpectedly accepted a protected target: {:?}",
                out.content
            );
            assert!(
                !serde_json::to_string(&out.content)
                    .unwrap()
                    .contains("protected bridge sentinel")
            );
        }
        let out = registry
            .get("grep")
            .unwrap()
            .run(
                json!({"path":project.path(),"pattern":"protected bridge sentinel"}),
                &ctx,
            )
            .await;
        assert!(
            !serde_json::to_string(&out.content)
                .unwrap()
                .contains("selected.txt:1:")
        );
        assert_eq!(
            std::fs::read_to_string(protected).unwrap(),
            "protected bridge sentinel"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn appended_project_skill_and_role_context_never_reads_protected_targets() {
        use std::os::unix::fs::symlink;
        let (engine, home, _rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        let protected = home.path().join("protected.md");
        std::fs::write(&protected, "protected context sentinel").unwrap();
        symlink(&protected, project.path().join("AGENTS.md")).unwrap();
        assert!(
            !crate::prompt::project_context(project.path(), &engine.0.protected_paths())
                .contains("protected context sentinel")
        );
        let roles = project.path().join(".tessera/agents");
        std::fs::create_dir_all(&roles).unwrap();
        symlink(&protected, roles.join("bad.md")).unwrap();
        assert!(
            crate::agents::discover(
                project.path(),
                &project.path().join("user-roles"),
                &engine.0.protected_paths()
            )
            .iter()
            .all(|d| !d.prompt.contains("protected context sentinel"))
        );
        let skills = home.path().join("protected-skills/test");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("SKILL.md"),
            "---\nname: protected\ndescription: protected context sentinel\n---\nbody",
        )
        .unwrap();
        let skillroot = project.path().join("skills");
        symlink(skills.parent().unwrap(), &skillroot).unwrap();
        assert!(
            crate::skills::discover_protected(
                &[(skillroot, "project".into())],
                &[],
                &engine.0.protected_paths()
            )
            .is_empty()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_authorizes_swapped_targets_before_reading_any_bytes() {
        use std::io::Seek;
        use std::os::unix::fs::symlink;

        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        // Only a stand-in under this throwaway Tessera store, never account files.
        let protected = home.path().join("notes.txt");
        std::fs::write(&protected, "protected snapshot sentinel").unwrap();
        for parent_swap in [false, true] {
            let work = project
                .path()
                .join(if parent_swap { "parent" } else { "final" });
            std::fs::create_dir(&work).unwrap();
            let path = work.join("notes.txt");
            std::fs::write(&path, "approved ordinary notes").unwrap();
            let ctx = snapshot_ctx(&engine, &rt, project.path(), 10);
            let input = json!({"file_path": path, "old_string": "notes", "new_string": "edit"});
            assert!(matches!(
                engine
                    .0
                    .claude_policy(&rt, &ctx.cwd, "Edit", &input, &json!({})),
                Policy::Allow
            ));
            // Deterministic policy/open interleaving: the checked pathname
            // now resolves to a protected object (final or directory link).
            if parent_swap {
                std::fs::rename(&work, project.path().join("saved-parent")).unwrap();
                symlink(home.path(), &work).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
                symlink(&protected, &path).unwrap();
            }
            let mut opened = crate::tools::file_object::Opened::open(&path).unwrap();
            assert!(snapshot_opened_before_edit(&ctx, &path, &opened).is_err());
            assert_eq!(
                opened.file.stream_position().unwrap(),
                0,
                "protected bytes read before authorization"
            );
            assert!(snapshot_before_edit(&ctx, &path).is_err());
            assert!(
                engine
                    .0
                    .store
                    .snapshots_since(&ctx.session_id, 0)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_keeps_the_opened_object_when_its_path_is_replaced_or_moved() {
        use std::io::Seek;
        use std::os::unix::fs::symlink;

        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let protected = home.path().join("protected.txt");
        std::fs::write(&protected, "protected snapshot sentinel").unwrap();
        for moved_to_store in [false, true] {
            let path = project.path().join(if moved_to_store {
                "moved.txt"
            } else {
                "replaced.txt"
            });
            std::fs::write(&path, "ordinary before edit").unwrap();
            let ctx = snapshot_ctx(&engine, &rt, project.path(), 20);
            let mut opened = crate::tools::file_object::Opened::open(&path).unwrap();
            crate::tools::files::authorize_file(&opened.path, &ctx, false).unwrap();
            let saved = if moved_to_store {
                home.path().join("moved.txt")
            } else {
                project.path().join("saved.txt")
            };
            std::fs::rename(&path, saved).unwrap();
            symlink(&protected, &path).unwrap();
            assert!(snapshot_opened_before_edit(&ctx, &path, &opened).is_err());
            assert_eq!(opened.file.stream_position().unwrap(), 0);
            assert!(
                engine
                    .0
                    .store
                    .snapshots_since(&ctx.session_id, 0)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn snapshot_preserves_regular_missing_oversized_and_cancelled_cases() {
        let (engine, home, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let ctx = snapshot_ctx(&engine, &rt, project.path(), 30);
        let path = project.path().join("ordinary.txt");
        let original = b"ordinary before edit\0binary";
        std::fs::write(&path, original).unwrap();
        snapshot_before_edit(&ctx, &path).unwrap();
        let new = project.path().join("new.txt");
        snapshot_before_edit(&ctx, &new).unwrap();
        let big = project.path().join("big.txt");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_SNAPSHOT + 1)
            .unwrap();
        snapshot_before_edit(&ctx, &big).unwrap();
        // Missing protected destinations must not get even a deletion snapshot.
        assert!(snapshot_before_edit(&ctx, &home.path().join("missing.txt")).is_err());
        ctx.cancel.cancel();
        assert!(snapshot_before_edit(&ctx, &path).is_err());
        let snapshots = engine.0.store.snapshots_since(&ctx.session_id, 0).unwrap();
        assert_eq!(
            snapshots,
            vec![
                (path.to_string_lossy().into_owned(), Some(original.to_vec())),
                (new.to_string_lossy().into_owned(), None),
            ]
        );
    }
    #[cfg(unix)]
    fn swapped_launch_directory() -> (
        tempfile::TempDir,
        crate::tools::file_object::ProcessDirectory,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(path.join("marker"), "checked").unwrap();
        std::fs::write(outside.join("marker"), "outside").unwrap();
        let directory = crate::tools::file_object::DirectoryScope::capture(&path)
            .unwrap()
            .open()
            .unwrap();
        std::fs::rename(&path, temp.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside, path).unwrap();
        (temp, directory)
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn full_and_one_shot_claude_commands_work_in_both_namespace_lanes() {
        if crate::sandbox::test_mode("Claude full and one-shot shared entry").is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap();
        std::fs::write(work.join("marker"), "checked").unwrap();
        let spec = (
            crate::sandbox::Spec {
                folders: vec![(work.clone(), true)],
                group: work.display().to_string(),
                ..Default::default()
            },
            true,
        );
        let directory = || crate::tools::file_object::ProcessDirectory::open(&work).unwrap();
        let mut proc = Proc::spawn(
            Path::new("/bin/sh"),
            &cc::Account::default(),
            vec![
                "-c".into(),
                "read init; cat marker; echo; read next; echo turn:$next".into(),
            ],
            directory(),
            Some(&spec),
            None,
            "initialize".into(),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), proc.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("checked")
        );
        // An ordinary second message, not only launch/initialize.
        proc.wtx
            .as_ref()
            .unwrap()
            .send("normal-turn".into())
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), proc.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("turn:normal-turn")
        );
        assert!(proc.close().await.how.is_none());
        let (mut cmd, held) = claude_command(
            Path::new("/bin/sh"),
            &cc::Account::default(),
            vec!["-c".into(), "cat marker".into()],
            directory(),
            Some(&spec),
            &[],
            false,
            false,
        )
        .unwrap();
        let child = crate::platform::spawn(&mut cmd, crate::platform::Start::Agent).unwrap();
        let _held = held.after_spawn();
        let out = child.wait_with_output().await.unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"checked");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn claude_close_stops_spawn_owned_setsid_descendant() {
        if !crate::tools::bash::tests::cleanup_fixture(
            "engine::claude_code::tests::claude_close_stops_spawn_owned_setsid_descendant",
        ) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut proc = Proc::spawn(
            Path::new("/bin/sh"),
            &cc::Account::default(),
            vec![
                "-c".into(),
                "read init; setsid /bin/sleep 30 & echo $!; read finish || :".into(),
            ],
            crate::tools::file_object::ProcessDirectory::open(dir.path()).unwrap(),
            None,
            None,
            "initialize".into(),
            &[],
            false,
        )
        .unwrap();
        let leader = proc.child.id().unwrap() as i32;
        let descendant: i32 =
            tokio::time::timeout(std::time::Duration::from_secs(3), proc.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .parse()
                .unwrap();
        // Keep the leader unreaped and alive while the spawn-time observer
        // retains its setsid child; then close lets the fake exit politely.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            unsafe { (libc::getpgid(descendant), libc::getsid(descendant)) },
            (descendant, descendant)
        );
        let ended = tokio::time::timeout(std::time::Duration::from_secs(12), proc.close())
            .await
            .unwrap();
        assert!(
            ended.how.is_none(),
            "fake did not exit politely: {:?}",
            ended.how
        );
        crate::tools::bash::tests::reaped_disappearance(&[leader, descendant]);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn full_claude_launcher_enters_the_verified_object_after_a_name_swap() {
        let (temp, directory) = swapped_launch_directory();
        let mut proc = Proc::spawn(
            Path::new("/bin/sh"),
            &cc::Account::default(),
            vec![
                "-c".into(),
                "read init; cat marker; printf original > launched".into(),
            ],
            directory,
            None,
            None,
            "initialize".into(),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), proc.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("checked")
        );
        assert!(proc.close().await.how.is_none());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("saved/launched")).unwrap(),
            "original"
        );
        assert!(!temp.path().join("outside/launched").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn one_shot_claude_launcher_enters_the_verified_object_after_a_name_swap() {
        let (temp, directory) = swapped_launch_directory();
        let (mut cmd, _held) = claude_command(
            Path::new("/bin/sh"),
            &cc::Account::default(),
            vec!["-c".into(), "cat marker; printf original > launched".into()],
            directory,
            None,
            &[],
            false,
            false,
        )
        .unwrap();
        let out = crate::platform::spawn(&mut cmd, crate::platform::Start::Agent)
            .unwrap()
            .wait_with_output()
            .await
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "checked");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("saved/launched")).unwrap(),
            "original"
        );
        assert!(!temp.path().join("outside/launched").exists());
    }

    #[tokio::test]
    async fn plan_and_explore_bridge_calls_cannot_change_machines_or_rename() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"actor\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        let (node, _events) = engine.node_conn("actor-machine");
        engine
            .node_request(
                Request::NodeHello {
                    name: "Fixture".into(),
                    os: "linux".into(),
                    version: "fixture".into(),
                    scope: tessera_proto::MachineScope {
                        commands: true,
                        ..Default::default()
                    },
                    generation: 1,
                    sandbox: String::new(),
                    sandbox_ok: false,
                    home: None,
                    computer: None,
                    host: None,
                    features: None,
                    guard_key: None,
                },
                &node,
            )
            .await
            .unwrap();
        let mut cfg = crate::config::LspConfig::default();
        cfg.command.insert("rust".into(), vec!["/bin/true".into()]);
        *engine.0.lsp.write() = Some(crate::lsp::LspManager::with_spawner(
            cfg,
            Box::new(|_, _, _| Err("fixture must not start an LSP".into())),
        ));
        engine
            .0
            .registry
            .write()
            .add(Arc::new(crate::tools::lsp::Lsp));
        rt.st.lock().internal.lsp = true;
        for actor in [
            json!({"agent_type":"Plan"}),
            json!({"agent_type":"Explore"}),
            json!({"permission_mode":"plan"}),
        ] {
            for (name, args) in [
                ("machines", json!({"action":"run", "command":"true"})),
                (
                    "lsp",
                    json!({"action":"rename", "path":"example.rs", "line":1, "symbol":"old", "new_name":"new"}),
                ),
            ] {
                let (mut turn, cwd) = {
                    let st = rt.st.lock();
                    let cwd = PathBuf::from(&st.info.cwd);
                    (
                        Turn::new(
                            st.info.id.clone(),
                            st.info.model.clone(),
                            cwd.clone(),
                            PermissionMode::Auto,
                            None,
                        ),
                        cwd,
                    )
                };
                let tools = engine.0.claude_bridge_tools(&rt.st.lock());
                assert!(tools.iter().any(|tool| tool.name == name));
                turn.bridge = Some(Bridge {
                    tools,
                    done: Default::default(),
                    cancel: CancellationToken::new(),
                });
                turn.open.push(("actor-call".into(), name.into()));
                let mut hook = actor.clone();
                hook["tool_name"] = json!(format!("mcp__tessera__{name}"));
                hook["input"] = args.clone();
                turn.policy_hooks.insert("actor-call".into(), hook.clone());
                assert!(matches!(
                    engine.0.claude_policy(
                        &rt,
                        &cwd,
                        &format!("mcp__tessera__{name}"),
                        &args,
                        &hook
                    ),
                    Policy::Deny(_)
                ));
                let (tx, mut rx) = mpsc::unbounded_channel();
                engine.0.claude_mcp(&rt, &mut turn, "request".into(), &json!({"server_name": cc::SERVER, "message": {"id":1, "method":"tools/call", "params": {"name":name, "arguments":args, "_meta":{"claudecode/toolUseId":"actor-call"}}}}), Some(&tx));
                let line = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let reply: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    reply.pointer("/response/response/mcp_response/result/isError"),
                    Some(&json!(true)),
                    "{reply}"
                );
                assert!(line.contains("read-only"), "{line}");
            }
        }
    }

    #[tokio::test]
    async fn read_only_actor_floor_reaches_mixed_tool_dispatch() {
        let (engine, _dir, rt) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        rt.st.lock().info.cwd = project.path().display().to_string();
        let sid = rt.st.lock().info.id.clone();
        let host = Host(engine.0.clone(), true);
        assert_eq!(host.permission(&sid), PermissionMode::ReadOnly);
        assert_eq!(rt.st.lock().info.permission, PermissionMode::Auto);
        assert!(
            host.note_save(&sid, None, "Fixture", "Plain note".into(), None)
                .unwrap_err()
                .contains("read-only")
        );
        assert!(
            host.note_picture_put(&sid, b"fixture")
                .unwrap_err()
                .contains("read-only")
        );
        for req in [
            json!({"agent_type":"Plan"}),
            json!({"agent_type":"Explore"}),
            json!({"permission_mode":"plan"}),
        ] {
            assert!(actor_is_read_only(&req));
            for (tool, args) in [
                (
                    Arc::new(crate::tools::machines::Machines) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"run", "command":"true"}),
                ),
                (
                    Arc::new(crate::tools::note::Note) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"create", "title":"Fixture", "content":"Plain note"}),
                ),
                (
                    Arc::new(crate::tools::meta::MemoryTool) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"add", "target":"user", "content":"Prefers plain notes"}),
                ),
                (
                    Arc::new(crate::tools::meta::SkillManage) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"create", "name":"fixture"}),
                ),
                (
                    Arc::new(crate::tools::meta::Schedule) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"end"}),
                ),
                (
                    Arc::new(crate::tools::settings::Settings) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"set", "key":"memory.enabled", "value":false}),
                ),
                (
                    Arc::new(crate::tools::lsp::Lsp) as Arc<dyn crate::tools::Tool>,
                    json!({"action":"rename", "path":"example.rs", "line":1, "symbol":"old", "new_name":"new"}),
                ),
            ] {
                let out = engine
                    .0
                    .execute_tool_ext(
                        &rt,
                        tool,
                        "actor-call",
                        args,
                        0,
                        &CancellationToken::new(),
                        false,
                        actor_is_read_only(&req),
                    )
                    .await;
                assert!(out.is_error);
                assert!(plain_text(&out.content).contains("read-only"), "{out:?}");
            }
        }
        let out = engine
            .0
            .execute_tool_ext(
                &rt,
                Arc::new(crate::tools::machines::Machines),
                "read-call",
                json!({"action":"list"}),
                0,
                &CancellationToken::new(),
                false,
                true,
            )
            .await;
        assert!(!out.is_error, "{out:?}");
        // A parent's read-only setting cannot be widened by an ordinary actor.
        rt.st.lock().info.permission = PermissionMode::ReadOnly;
        assert_eq!(
            Host(engine.0.clone(), false).permission(&sid),
            PermissionMode::ReadOnly
        );
    }

    #[test]
    fn lite_background_count_includes_calls_without_task_ids_and_wakeups() {
        let mut t = Turn::new(
            "s".into(),
            "claude-code/opus".parse().unwrap(),
            PathBuf::from("/tmp"),
            PermissionMode::Auto,
            None,
        );
        for n in 0..11 {
            t.agents.insert(
                format!("call-{n}"),
                AgentRun {
                    background: true,
                    ..Default::default()
                },
            );
        }
        assert_eq!(t.background_count(), 11);
        // The task event may omit its background flag, or arrive later.
        t.tasks.insert("agent-task".into(), ("call-0".into(), true));
        t.tasks
            .insert("bash-task".into(), ("bash-call".into(), true));
        t.tasks
            .insert("monitor-task".into(), ("monitor-call".into(), true));
        t.crons = 2;
        assert_eq!(t.background_count(), 15); // no duplicate agent count
        t.tasks.clear();
        t.crons = 0;
        for a in t.agents.values_mut() {
            a.done = true;
        }
        assert_eq!(t.background_count(), 0);
        let mut wait = BackgroundWait::default();
        assert!(wait.count_changed(11, true));
        assert!(!wait.count_changed(11, true));
        assert!(wait.count_changed(0, false));
    }
    fn lite_turn(sid: String) -> Turn {
        Turn::new(
            sid,
            "claude-code/opus".parse().unwrap(),
            PathBuf::from("/tmp"),
            PermissionMode::Auto,
            None,
        )
    }

    #[test]
    fn lite_result_identities_aliases_and_terminal_replay_are_canonical() {
        for name in ["Agent", "Task", "Monitor", "Bash", "PowerShell"] {
            let mut t = lite_turn("s".into());
            if cc::is_agent(name) {
                t.agents.insert(
                    "call".into(),
                    AgentRun {
                        requested_background: true,
                        ..Default::default()
                    },
                );
                assert_eq!(t.background_count(), 0, "request alone is not launch");
            }
            t.background_result(
                "call",
                name,
                &json!({"tool_use_result":{"agentId":"agent", "backgroundTaskId":"task"}}),
                false,
            );
            t.link_background("task".into(), String::new(), false);
            assert_eq!(
                t.tasks["task"],
                ("call".into(), true),
                "missing flag/call preserves known identity"
            );
            assert_eq!(t.background_count(), 1, "aliases count once");
            t.finished = true;
            assert!(t.finish_background(Some("task"), None));
            assert!(t.carry_on_expected);
            if let Some(a) = t.agents.get_mut("call") {
                a.done = true;
            }
            t.retire_agents();
            assert_eq!(t.background_count(), 0);
            t.carry_on_expected = false;
            assert!(!t.finish_background(Some("task"), None));
            t.link_background("task".into(), "call".into(), true);
            t.link_background("late-alias".into(), "call".into(), true);
            t.background_result(
                "call",
                name,
                &json!({"tool_use_result":{"backgroundTaskId":"task"}}),
                false,
            );
            assert_eq!(
                t.background_count(),
                0,
                "late start/result cannot resurrect work"
            );
            assert!(
                !t.carry_on_expected,
                "duplicate completion cannot extend grace"
            );
        }
    }

    #[tokio::test]
    async fn lite_rejected_background_agent_saves_error_then_retires() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        for started in [false, true] {
            let mut t = lite_turn(rt.st.lock().info.id.clone());
            engine.0.claude_agent_start(
                &rt,
                &mut t,
                "rejected",
                &json!({"run_in_background":true}),
            );
            let child = t.agents["rejected"]
                .child
                .as_ref()
                .unwrap()
                .st
                .lock()
                .info
                .id
                .clone();
            t.open.push(("rejected".into(), "Agent".into()));
            if started {
                t.link_background("task".into(), "rejected".into(), true);
            }
            engine.0.claude_message(&rt, &mut t, json!({"type":"user", "message":{"content":[{"type":"tool_result","tool_use_id":"rejected","is_error":true,"content":"denied before launch"}]}}), None, &CancellationToken::new());
            assert!(t.agents.is_empty() && t.tasks.is_empty());
            assert_eq!(t.background_count(), 0);
            assert_eq!(
                engine
                    .0
                    .store
                    .get_session(&child)
                    .unwrap()
                    .unwrap()
                    .0
                    .status,
                SessionStatus::Error
            );
        }
    }

    #[tokio::test]
    async fn lite_repeated_wakeup_helper_cycles_retire_saved_state_and_keep_terminal_ids() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        let mut t = lite_turn(rt.st.lock().info.id.clone());
        for n in 0..512 {
            let call = format!("cycle-{n}");
            let task = format!("task-{n}");
            t.crons = 1;
            engine
                .0
                .claude_agent_start(&rt, &mut t, &call, &json!({"run_in_background":true}));
            let child = t.agents[&call]
                .child
                .as_ref()
                .unwrap()
                .st
                .lock()
                .info
                .id
                .clone();
            t.call_args
                .insert(call.clone(), json!({"run_in_background":true}));
            t.calls.insert(call.clone(), n as u64);
            t.background_result(
                &call,
                "Agent",
                &json!({"tool_use_result":{"agentId":task}}),
                false,
            );
            assert_eq!(t.background_count(), 2);
            engine.0.claude_system(
                &rt,
                &mut t,
                &json!({"subtype":"task_notification","task_id":task,"status":"completed"}),
                None,
            );
            assert_eq!(
                engine
                    .0
                    .store
                    .get_session(&child)
                    .unwrap()
                    .unwrap()
                    .0
                    .status,
                SessionStatus::Idle
            );
            t.crons = 0;
            assert!(
                t.agents.is_empty()
                    && t.tasks.is_empty()
                    && t.call_args.is_empty()
                    && t.calls.is_empty()
            );
            assert!(t.terminal_background.contains(&call));
            assert!(t.terminal_background.contains(&task));
            assert_eq!(t.background_count(), 0);
        }
        assert_eq!(t.terminal_background.len(), 1024);
        for n in 0..1024 {
            t.background_result(&format!("unrelated-{n}"), "Read", &Value::Null, false);
        }
        assert!(t.terminal_background.contains("cycle-0"));
        assert!(t.terminal_background.contains("task-0"));
        t.link_background("task-0".into(), "cycle-0".into(), true);
        t.background_result(
            "cycle-0",
            "Agent",
            &json!({"tool_use_result":{"backgroundTaskId":"task-0","agentId":"late-alias"}}),
            false,
        );
        assert!(t.terminal_background.contains("late-alias"));
        assert_eq!(t.background_count(), 0);
        assert!(!t.retain_background());
        assert!(t.tasks.is_empty());
    }

    #[tokio::test]
    async fn lite_foreground_agent_id_is_correlation_not_background_launch() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        for (name, started) in [
            ("Agent", false),
            ("Agent", true),
            ("Task", false),
            ("Task", true),
        ] {
            let mut t = lite_turn(rt.st.lock().info.id.clone());
            engine.0.claude_agent_start(
                &rt,
                &mut t,
                "foreground",
                &json!({"run_in_background":false}),
            );
            let child = t.agents["foreground"]
                .child
                .as_ref()
                .unwrap()
                .st
                .lock()
                .info
                .id
                .clone();
            t.open.push(("foreground".into(), name.into()));
            t.call_args
                .insert("foreground".into(), json!({"run_in_background":false}));
            t.calls.insert("foreground".into(), 1);
            if started {
                engine.0.claude_system(&rt, &mut t, &json!({"subtype":"task_started","task_id":"foreground-task","tool_use_id":"foreground","is_backgrounded":false}), None);
                assert_eq!(t.tasks["foreground-task"], ("foreground".into(), false));
                assert!(!t.agents["foreground"].background);
            }
            engine.0.claude_message(&rt, &mut t, json!({"type":"user","tool_use_result":{"agentId":"foreground-id"},"message":{"content":[{"type":"tool_result","tool_use_id":"foreground","content":"done"}]}}), None, &CancellationToken::new());
            assert_eq!(t.background_count(), 0);
            assert!(!t.retain_background());
            assert!(t.agents.is_empty() && t.tasks.is_empty());
            assert!(t.call_args.is_empty() && t.calls.is_empty());
            assert!(t.terminal_background.contains("foreground-id"));
            if started {
                assert!(t.terminal_background.contains("foreground-task"));
            }
            assert_eq!(
                engine
                    .0
                    .store
                    .get_session(&child)
                    .unwrap()
                    .unwrap()
                    .0
                    .status,
                SessionStatus::Idle
            );
        }
    }

    #[tokio::test]
    async fn lite_task_only_completion_before_launch_result_terminalizes_all_aliases() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        for alias in [false, true] {
            let mut t = lite_turn(rt.st.lock().info.id.clone());
            engine
                .0
                .claude_agent_start(&rt, &mut t, "pending", &json!({"run_in_background":true}));
            let child = t.agents["pending"]
                .child
                .as_ref()
                .unwrap()
                .st
                .lock()
                .info
                .id
                .clone();
            t.open.push(("pending".into(), "Agent".into()));
            // Neither a start nor a call correlation has reached Tessera yet.
            engine.0.claude_system(
                &rt,
                &mut t,
                &json!({"subtype":"task_notification","task_id":"early-task","status":"completed"}),
                None,
            );
            assert!(t.retain_background(), "pending call is not correlated yet");
            let mut result = json!({"backgroundTaskId":"early-task"});
            if alias {
                result["agentId"] = json!("distinct-agent-alias");
            }
            engine.0.claude_message(&rt, &mut t, json!({"type":"user","tool_use_result":result,"message":{"content":[{"type":"tool_result","tool_use_id":"pending","content":"Launched"}]}}), None, &CancellationToken::new());
            assert_eq!(t.background_count(), 0);
            assert!(!t.retain_background());
            assert!(t.agents.is_empty() && t.tasks.is_empty());
            assert!(t.terminal_background.contains("early-task"));
            assert!(t.terminal_background.contains("pending"));
            if alias {
                assert!(t.terminal_background.contains("distinct-agent-alias"));
                t.link_background("distinct-agent-alias".into(), "new-call".into(), true);
                assert!(t.terminal_background.contains("new-call"));
            }
            assert!(t.tasks.is_empty());
            assert_eq!(
                engine
                    .0
                    .store
                    .get_session(&child)
                    .unwrap()
                    .unwrap()
                    .0
                    .status,
                SessionStatus::Idle
            );
        }
    }

    #[tokio::test]
    async fn lite_terminal_start_ends_pending_agent_without_waiting_for_a_launch_result() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        let mut t = lite_turn(rt.st.lock().info.id.clone());
        engine
            .0
            .claude_agent_start(&rt, &mut t, "pending", &json!({"run_in_background":true}));
        let child = t.agents["pending"]
            .child
            .as_ref()
            .unwrap()
            .st
            .lock()
            .info
            .id
            .clone();
        t.open.push(("pending".into(), "Agent".into()));
        engine.0.claude_system(
            &rt,
            &mut t,
            &json!({"subtype":"task_notification","task_id":"early-task","status":"completed"}),
            None,
        );
        engine.0.claude_system(&rt, &mut t, &json!({"subtype":"task_started","task_id":"early-task","tool_use_id":"pending","is_backgrounded":true}), None);
        assert_eq!(t.background_count(), 0);
        assert!(!t.retain_background());
        assert!(t.tasks.is_empty());
        assert!(t.agents["pending"].done);
        assert!(t.terminal_background.contains("pending"));
        assert_ne!(
            engine
                .0
                .store
                .get_session(&child)
                .unwrap()
                .unwrap()
                .0
                .status,
            SessionStatus::Running
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lite_unresolved_background_request_retains_without_confirmed_count_until_the_cap() {
        let mut t = lite_turn("s".into());
        t.agents.insert(
            "pending".into(),
            AgentRun {
                requested_background: true,
                ..Default::default()
            },
        );
        assert_eq!(t.background_count(), 0);
        assert!(t.retain_background());
        assert!(!gone_quiet(t.retain_background(), BACKGROUND_MAX, false));
        let mut wait = BackgroundWait::default();
        wait.start();
        assert!(wait.count_changed(0, true));
        assert!(!wait.count_changed(0, true));
        tokio::time::advance(BACKGROUND_MAX).await;
        assert_eq!(wait.tick(false), BackgroundTick::Expired);
        t.finish_background(None, Some("pending"));
        assert!(!t.retain_background());
        assert!(wait.count_changed(0, false));
    }

    #[tokio::test]
    async fn lite_only_parent_rosters_end_wakeup_wait_and_grace_is_ten_minutes() {
        let (engine, _home, rt) = super::super::test_support::engine().await;
        let mut t = lite_turn(rt.st.lock().info.id.clone());
        let (tx, _rx) = mpsc::unbounded_channel();
        t.crons = 1;
        t.finished = true;
        engine.0.claude_hook(&rt, &mut t, "child".into(), &json!({"callback_id":cc::HOOK_STOP,"input":{"stop_hook_active":true,"agent_id":"child","session_crons":[]}}), Some(&tx));
        assert_eq!(t.crons, 1);
        assert!(!t.carry_on_expected);
        let parent = json!({"callback_id":cc::HOOK_STOP,"input":{"stop_hook_active":true,"session_crons":[]}});
        engine
            .0
            .claude_hook(&rt, &mut t, "parent".into(), &parent, Some(&tx));
        assert_eq!(t.crons, 0);
        assert!(t.carry_on_expected);
        assert!(!gone_quiet(
            false,
            Duration::from_secs(599),
            t.carry_on_expected
        ));
        assert!(gone_quiet(
            false,
            Duration::from_secs(600),
            t.carry_on_expected
        ));
        t.carry_on_expected = false;
        engine
            .0
            .claude_hook(&rt, &mut t, "replay".into(), &parent, Some(&tx));
        assert!(!t.carry_on_expected, "zero-to-zero is not a new completion");
    }

    #[tokio::test(start_paused = true)]
    async fn stale_context_does_not_bypass_background_expiry() {
        let mut wait = BackgroundWait::default();
        wait.start();
        assert_eq!(wait.tick(false), BackgroundTick::Current);
        assert_eq!(wait.tick(true), BackgroundTick::Stale);
        tokio::time::advance(BACKGROUND_MAX).await;
        assert_eq!(wait.tick(true), BackgroundTick::Expired);
        assert_eq!(wait.tick(false), BackgroundTick::Expired);
    }

    /// What broke an overnight run: its turn ended with subagents working,
    /// a background command finished after a few minutes and Claude Code
    /// carried on by itself; 15 minutes after the first end Tessera closed
    /// its input mid-work, and every approval after that failed.
    #[tokio::test(start_paused = true)]
    async fn the_wait_starts_over_when_claude_code_carries_on() {
        let min = |m: u64| std::time::Duration::from_secs(m * 60);
        let mut w = BackgroundWait::default();
        w.start();
        tokio::time::advance(min(5)).await;
        // Subagent chatter doesn't count as carrying on.
        assert!(!w.heard(&json!({"type": "assistant", "parent_tool_use_id": "toolu_ag"})));
        assert!(!w.heard(&json!({"type": "system", "subtype": "task_notification"})));
        assert!(w.waiting());
        // Its own turn again: busy, not waiting.
        assert!(w.heard(&json!({"type": "assistant", "parent_tool_use_id": null})));
        assert!(!w.waiting());
        tokio::time::advance(min(30)).await;
        assert!(!w.expired(), "never closed while it works");
        // That part ends with work still running: the clock starts over.
        w.start();
        // An hour-long upgrade or build is no reason to close its input
        // (a 15-minute limit broke a homelab upgrade's follow-up).
        tokio::time::advance(min(60)).await;
        assert!(!w.expired());
        tokio::time::advance(min(11 * 60)).await;
        assert!(w.expired(), "half a day at most");
        // Only the first end of a wait counts.
        let mut w = BackgroundWait::default();
        w.start();
        tokio::time::advance(min(6 * 60)).await;
        w.start();
        tokio::time::advance(min(6 * 60)).await;
        assert!(w.expired());
    }

    /// Right after the background work it waited for ended, Claude Code takes
    /// its turn up again; a slow first answer (a cold cache after an hour's
    /// build) must not read as the end of its run: its kept process would
    /// otherwise take that turn up outside the run.
    #[test]
    fn a_slow_start_after_background_work_is_not_the_end_of_the_run() {
        let s = std::time::Duration::from_secs;
        assert!(!gone_quiet(true, s(3600), false), "work left");
        assert!(!gone_quiet(false, s(10), false));
        assert!(gone_quiet(false, s(20), false));
        assert!(!gone_quiet(false, s(45), true), "it is about to carry on");
        assert!(!gone_quiet(false, s(9 * 60), true));
        assert!(gone_quiet(false, s(10 * 60), true), "not for ever");
    }

    /// A kept process's lines between turns: questions get Tessera's own
    /// plain refusal (never a Stop); its own turn is work for the chat.
    #[test]
    fn a_kept_process_hands_its_own_turn_over() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let ask = json!({"type": "control_request", "request_id": "p1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {}}});
        assert_eq!(parked_line(&ask, Some(&tx)), ParkedLine::Quiet);
        let answer: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(answer["response"]["response"]["behavior"], "deny");
        assert_eq!(answer["response"]["response"]["interrupt"], false);
        assert!(
            answer["response"]["response"]["message"]
                .as_str()
                .unwrap()
                .contains("no active turn")
        );
        for work in [
            json!({"type": "system", "subtype": "init", "session_id": "s"}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "message_start"}}),
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": []}}),
        ] {
            assert_eq!(parked_line(&work, Some(&tx)), ParkedLine::Work, "{work}");
        }
        for quiet in [
            json!({"type": "system", "subtype": "task_notification", "task_id": "t"}),
            json!({"type": "assistant", "parent_tool_use_id": "toolu_ag", "message": {}}),
            json!({"type": "rate_limit_event"}),
            json!({"type": "user", "isReplay": true, "isSynthetic": true, "message": {}}),
            json!({"type": "user", "isReplay": true, "message": {}}),
        ] {
            assert_eq!(parked_line(&quiet, Some(&tx)), ParkedLine::Quiet, "{quiet}");
        }
        assert!(rx.try_recv().is_err());
    }

    fn entry(seq: u64, body: EntryBody) -> Entry {
        Entry { seq, ts: seq, body }
    }

    fn user(seq: u64, text: &str) -> Entry {
        entry(
            seq,
            EntryBody::User {
                content: vec![ContentPart::text(text)],
                author: None,
            },
        )
    }

    fn reply(seq: u64, text: &str) -> Entry {
        reply_by(seq, text, None)
    }

    fn reply_by(seq: u64, text: &str, author: Option<&str>) -> Entry {
        entry(
            seq,
            EntryBody::Assistant {
                content: vec![AssistantPart::Text { text: text.into() }],
                model: ModelRef {
                    provider: "local".into(),
                    model: "m".into(),
                },
                usage: Usage::default(),
                stop: StopReason::Stop,
                error: None,
                author: author.map(str::to_string),
            },
        )
    }

    #[test]
    fn only_what_it_has_not_seen_is_sent() {
        let es = vec![user(1, "hi"), reply(2, "hello"), user(3, "next")];
        // It saw everything up to the reply: just the new message.
        let none = |_: &str| String::new();
        assert_eq!(prompt(&es, 2, &none), vec![ContentPart::text("next")]);
        // Another model answered before: that part comes along as context.
        let p = prompt(&es, 0, &none);
        assert_eq!(p.len(), 3);
        let ContentPart::Text { text } = &p[0] else {
            panic!()
        };
        assert!(text.contains("User: hi") && text.contains("Assistant: hello"));
        assert!(!text.contains("next"));
        assert_eq!(p[2], ContentPart::text("next"));
    }

    #[test]
    fn a_message_from_another_agent_says_who_wrote_it() {
        // A bot on Claude Code asked by another chat's agent (a bot DM).
        let es = vec![entry(
            1,
            EntryBody::User {
                content: vec![ContentPart::text("check the backups")],
                author: Some("chat:ses_1".into()),
            },
        )];
        let label = |a: &str| {
            if a == "chat:ses_1" {
                "the user's agent in the chat \"Server\"".to_string()
            } else {
                String::new()
            }
        };
        assert_eq!(
            prompt(&es, 0, &label),
            vec![ContentPart::text(
                "[the user's agent in the chat \"Server\"]: check the backups"
            )]
        );
        // The user's own words stay as they are.
        assert_eq!(
            prompt(&[user(1, "hi")], 0, &label),
            vec![ContentPart::text("hi")]
        );
    }

    #[test]
    fn in_a_channel_it_hears_the_others_by_name() {
        let es = vec![
            user(1, "@ada @bob plan the trip"),
            reply_by(2, "I'll book trains.", Some("b_ada")),
            reply_by(3, "Hotels from me.", Some("b_bob")),
            user(4, "thanks"),
        ];
        let handle = |id: &str| format!("@{}", id.trim_start_matches("b_"));
        let p = room_prompt(&es, 0, "b_bob", &handle);
        let ContentPart::Text { text } = &p[0] else {
            panic!()
        };
        assert!(text.contains("[user]: @ada @bob plan the trip"), "{text}");
        assert!(text.contains("[@ada]: I'll book trains."), "{text}");
        // Its own reply it knows already.
        assert!(!text.contains("Hotels from me"), "{text}");
        assert!(text.ends_with("[Your turn: reply as yourself.]"));
        // Later: only what is new since it last spoke.
        let p = room_prompt(&es, 3, "b_bob", &handle);
        let ContentPart::Text { text } = &p[0] else {
            panic!()
        };
        assert!(text.contains("[user]: thanks") && !text.contains("trains"));
    }

    #[test]
    fn transfer_keeps_old_images_and_verbatim_checkpoint_beyond_text_tail() {
        let image = ContentPart::Image {
            mime: "image/png".into(),
            data: "aW1hZ2U=".into(),
        };
        let mut es = vec![
            entry(
                1,
                EntryBody::User {
                    content: vec![image.clone()],
                    author: None,
                },
            ),
            entry(
                2,
                EntryBody::Compaction {
                    external: false,
                    summary: "A durable native checkpoint".into(),
                    first_kept_seq: 1,
                    tokens_before: 99,
                    tokens_after: 20,
                },
            ),
        ];
        for i in 3..35 {
            es.push(if i % 2 == 0 {
                user(i, &"large turn ".repeat(1200))
            } else {
                reply(i, "ok")
            });
        }
        es.push(user(35, "New instruction"));
        let p = prompt(&es, 0, &|_| String::new());
        assert!(
            p.contains(&image),
            "image is a real content part, not a stub"
        );
        let text = tessera_proto::plain_text(&p);
        assert!(text.contains("A durable native checkpoint"));
        assert!(text.contains("recall") && text.contains("omitted"));
        assert!(
            text.len() > 24000,
            "never cuts structured state to a character tail"
        );
    }

    #[test]
    fn ledger_clear_is_a_new_version_and_not_system_context() {
        let ledger = TaskLedger {
            goal: "Original".into(),
            constraints: vec!["No deploy".into()],
            ..Default::default()
        };
        let (v, text) = ledger_context(Some(&ledger));
        assert!(text.contains("Original") && text.contains("No deploy"));
        assert_eq!(v, ledger_context(Some(&ledger)).0);
        let (cleared, text) = ledger_context(None);
        assert_ne!(v, cleared);
        assert!(text.contains("cleared goal") && !text.contains("Original"));
    }

    #[test]
    fn bridged_tools_show_by_their_own_name() {
        let p = shown_part(AssistantPart::ToolCall {
            id: "t1".into(),
            name: "mcp__tessera__note".into(),
            arguments: json!({}),
        });
        assert!(matches!(p, AssistantPart::ToolCall { name, .. } if name == "note"));
        let p = shown_part(AssistantPart::ToolCall {
            id: "t2".into(),
            name: "mcp__github__issue".into(),
            arguments: json!({}),
        });
        assert!(matches!(p, AssistantPart::ToolCall { name, .. } if name == "mcp__github__issue"));
    }

    #[test]
    fn run_files_are_private_and_go_away() {
        let f = RunFiles::with_context("# Tessera\n").unwrap();
        let dir = f.dir.clone();
        assert_eq!(std::fs::read_to_string(&f.context).unwrap(), "# Tessera\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
            let mode = std::fs::metadata(&f.context).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        drop(f);
        assert!(!dir.exists());
    }
}
