//! Session history as rendered blocks: user messages, Markdown answers,
//! thinking, tool steps, subagent reports, notices and compaction dividers.
//!
//! Like the apps, a finished run of tool calls folds into one "N steps"
//! row (ctrl+o unfolds everything), three or more subagent reports in a row
//! fold into one "Reports" row, and thinking is a single quiet line.
//! Rendered lines are cached per item and only recomputed when their inputs
//! change.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use tessera_proto::presentation::ToolPresentation;
use tessera_proto::*;

use super::markdown;
use super::motif::{self, Sq};
use super::text::{self, span};
use super::theme::{Theme, Tok};

/// Live state of a tool call that is executing.
#[derive(Debug, Clone)]
pub struct RunningTool {
    pub name: String,
    pub output: String,
    pub started: Instant,
}

/// Everything besides the entries that affects rendering.
pub struct Env<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    pub expand: bool,
    pub thinking: bool,
    /// Step of the working indicators (see `motif::STEP_MS`).
    pub frame: usize,
    pub running: &'a HashMap<String, RunningTool>,
    pub approvals: &'a [ApprovalRequest],
    pub active: bool,
    pub now: Instant,
    pub children: &'a [SessionInfo],
    /// Session working directory: paths inside it are shown relative.
    pub cwd: &'a str,
    /// The session was interrupted (server restart): unanswered calls
    /// may have started.
    pub interrupted: bool,
    /// Known bots by id (names and colours for speaker labels).
    pub bots: &'a HashMap<String, BotLook>,
    /// The bot this chat is with: assistant entries without an author
    /// belong to it.
    pub bot: Option<&'a str>,
    /// Multi-speaker room: speakers are labelled, single tool calls fold.
    pub channel: bool,
    /// How long each assistant entry thought, by seq (measured live).
    pub think_ms: &'a HashMap<u64, u64>,
}

/// How a bot is shown in a chat that has one: its name in its colour.
#[derive(Debug, Clone)]
pub struct BotLook {
    pub name: String,
    pub handle: String,
    /// `#rrggbb` as configured (hashed into render keys).
    pub color: String,
    pub style: Style,
}

impl BotLook {
    pub fn new(b: &Bot, t: &Theme) -> Self {
        Self {
            name: b.name.clone(),
            handle: b.handle.clone(),
            color: b.color.clone(),
            style: t.bot(&b.color, &b.id),
        }
    }

    /// A dot in the bot's colour.
    pub fn avatar_spans(&self) -> Vec<Span<'static>> {
        vec![span("●", self.style)]
    }
}

/// Who wrote an entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Speaker {
    You,
    Bot(String),
    /// The plain assistant (no bot).
    Agent,
}

/// Header line naming a speaker.
pub fn speaker_line(sp: &Speaker, env: &Env<'_>, suffix: Option<&str>) -> Line<'static> {
    let t = env.theme;
    let mut spans = match sp {
        Speaker::You => vec![span("You", t.bold())],
        Speaker::Agent => vec![span("◆ ", t.accent()), span("Tessera", t.bold())],
        Speaker::Bot(id) => match env.bots.get(id) {
            Some(b) => {
                let mut v = b.avatar_spans();
                v.push(span(" ", Style::new()));
                v.push(span(
                    text::sanitize(&b.name).into_owned(),
                    b.style.add_modifier(Modifier::BOLD),
                ));
                v.push(span(format!("  @{}", b.handle), t.faint()));
                v
            }
            // Not a bot: another chat's agent, a scheduled job, or a bot
            // since removed.
            None if id.starts_with("chat:") => {
                vec![span("◆ ", t.accent()), span("A chat's agent", t.bold())]
            }
            None if id.starts_with("job:") => {
                vec![span("◆ ", t.faint()), span("Scheduled job", t.muted())]
            }
            None => vec![span("◆ ", t.faint()), span("bot", t.muted())],
        },
    };
    if let Some(s) = suffix {
        spans.push(span(format!("  {s}"), t.dim()));
    }
    text::truncate_line(Line::from(spans), env.width)
}

impl Env<'_> {
    /// The working indicator (three tesserae).
    pub fn spinner(&self) -> Vec<Span<'static>> {
        motif::spinner(self.theme, self.frame, 3)
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Entry(Entry),
    /// Client-side notice (request errors etc.), shown after entry `after`.
    Local {
        after: u64,
        n: u32,
        level: NoticeLevel,
        text: String,
    },
}

impl Item {
    fn key(&self) -> (u64, u32) {
        match self {
            Item::Entry(e) => (e.seq, 0),
            Item::Local { after, n, .. } => (*after, *n),
        }
    }
}

struct Rendered {
    key: u64,
    lines: Vec<Line<'static>>,
}

/// A block's shape, for spacing: rows (tool steps, a collapsed thought)
/// stack tightly; prose gets a blank line around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Row,
    Prose,
}

/// A folded run of steps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StepsRow {
    states: Vec<Sq>,
    tools: Vec<String>,
    ms: Option<u64>,
    failed: usize,
}

/// A folded run of subagent reports.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReportsRow {
    states: Vec<Sq>,
}

/// How an item takes part in folding (computed for every item on each
/// `prepare`; it is part of the render key).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct Plan {
    /// The item's steps are folded into a group.
    fold: bool,
    /// The item starts a folded group: it draws the group's row.
    head: Option<StepsRow>,
    /// A report folded into a "Reports" row.
    report_fold: bool,
    report_head: Option<ReportsRow>,
    /// No blank line above (rows following rows).
    tight: bool,
    /// A completion check answered quietly (and the answer): not shown.
    hidden: bool,
}

/// A run of steps being collected by `Transcript::plan`.
#[derive(Default)]
struct StepRun {
    items: Vec<usize>,
    states: Vec<Sq>,
    tools: Vec<String>,
    ms: Option<u64>,
    min_tools: usize,
    /// Folding is on (ctrl+o turns it off).
    fold: bool,
    housekeeping: bool,
}

impl StepRun {
    fn add_ms(&mut self, ms: Option<u64>) {
        if let Some(ms) = ms {
            *self.ms.get_or_insert(0) += ms;
        }
    }

    /// End the run: a finished run with enough tool calls folds into one
    /// row, drawn by its first item.
    fn flush(&mut self, plans: &mut [Plan], finished: bool) {
        if finished
            && self.fold
            && (self.tools.len() >= self.min_tools || self.housekeeping)
            && !self.items.is_empty()
        {
            for i in &self.items {
                plans[*i].fold = true;
            }
            let failed = self.states.iter().filter(|s| **s == Sq::Failed).count();
            plans[self.items[0]].head = Some(StepsRow {
                states: std::mem::take(&mut self.states),
                tools: std::mem::take(&mut self.tools),
                ms: self.ms,
                failed,
            });
        }
        self.items.clear();
        self.states.clear();
        self.tools.clear();
        self.ms = None;
        self.housekeeping = false;
    }
}

#[derive(Default)]
pub struct Transcript {
    items: Vec<Item>,
    rendered: Vec<Option<Rendered>>,
    /// call_id → seq of its ToolResult.
    results: HashMap<String, u64>,
    /// call ids that appear in assistant entries.
    calls: HashSet<String>,
    local_n: u32,
    /// Speaker of the last message (after `prepare`).
    last_speaker: Option<Speaker>,
    /// Observed execution intervals persisted in result details (ms).
    durations: HashMap<String, u64>,
    /// Subagent id → its report said it failed.
    reports: HashMap<String, bool>,
    /// `durations` and `reports` need recomputing.
    derived_stale: bool,
    #[cfg(test)]
    renders: usize,
}

enum CallState<'a> {
    /// Arguments are still streaming from the model.
    Streaming,
    Pending,
    Approval,
    Running {
        output: &'a str,
        elapsed: Duration,
    },
    Done {
        content: &'a [ContentPart],
        is_error: bool,
        details: Option<&'a Value>,
        seq: u64,
    },
    NotRun,
}

impl CallState<'_> {
    fn square(&self) -> Sq {
        match self {
            CallState::Streaming | CallState::Running { .. } => Sq::Running,
            CallState::Pending => Sq::Waiting,
            CallState::Approval => Sq::Approval,
            CallState::Done {
                details: Some(d), ..
            } if d.get("interrupted").and_then(Value::as_bool) == Some(true)
                || d.get("stopped").and_then(Value::as_bool) == Some(true)
                || d.get("status").and_then(Value::as_str) == Some("stopped")
                || d.pointer("/subagent/status").and_then(Value::as_str) == Some("stopped") =>
            {
                Sq::Unknown
            }
            CallState::Done {
                is_error: false, ..
            } => Sq::Done,
            CallState::Done { is_error: true, .. } => Sq::Failed,
            CallState::NotRun => Sq::Unknown,
        }
    }
}

/// The engine's "… — retrying in 4s (attempt 2/5)" notice.
fn is_retry(t: &str) -> bool {
    t.contains(" — retrying in ") && t.contains("(attempt ")
}

/// Special authors of "user" entries the server writes.
fn report_child(author: Option<&str>) -> Option<&str> {
    author?.strip_prefix("subagent:")
}

fn is_system_author(author: Option<&str>) -> bool {
    matches!(author, Some("goal" | "harness")) || report_child(author).is_some()
}

/// "[Completion check] …": the harness's question before a turn ends
/// (`tools::meta::completion_note`).
fn is_completion_check(e: &Entry) -> bool {
    matches!(&e.body, EntryBody::User { content, author: Some(a) }
        if a == "harness" && plain_text(content).trim_start().starts_with("[Completion check]"))
}

/// Whether `e` belongs to a quiet answer to a completion check (ledger
/// updates and a bare ✓); `None`: it isn't part of an answer at all.
fn quiet_reply(e: &Entry) -> Option<bool> {
    match &e.body {
        EntryBody::ToolResult { .. } => Some(true),
        EntryBody::Assistant {
            content,
            error,
            stop,
            ..
        } => Some(
            error.is_none()
                && !matches!(
                    stop,
                    StopReason::Aborted | StopReason::Length | StopReason::Error
                )
                && content.iter().all(quiet_part),
        ),
        _ => None,
    }
}

/// A part of a quiet answer to a completion check.
pub fn quiet_part(p: &AssistantPart) -> bool {
    match p {
        AssistantPart::Thinking { .. } => true,
        AssistantPart::ToolCall { name, .. } => name == "task",
        AssistantPart::Text { text } => {
            text.trim().is_empty() || tessera_core::tools::meta::is_check_ack(text)
        }
    }
}

impl Transcript {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.items.iter().filter_map(|i| match i {
            Item::Entry(e) => Some(e),
            _ => None,
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.items
            .iter()
            .rev()
            .find_map(|i| match i {
                Item::Entry(e) => Some(e.seq),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn find(&self, seq: u64) -> Option<&Entry> {
        let idx = self.items.binary_search_by_key(&(seq, 0), Item::key).ok()?;
        match &self.items[idx] {
            Item::Entry(e) => Some(e),
            _ => None,
        }
    }

    /// Insert an entry (deduplicated by seq). Returns true when it was new.
    pub fn add_entry(&mut self, entry: Entry) -> bool {
        let key = (entry.seq, 0);
        let pos = match self.items.binary_search_by_key(&key, Item::key) {
            Ok(_) => return false,
            Err(p) => p,
        };
        match &entry.body {
            EntryBody::Assistant { content, .. } => {
                let mut invalidate = Vec::new();
                for p in content {
                    if let AssistantPart::ToolCall { id, .. } = p {
                        self.calls.insert(id.clone());
                        if let Some(seq) = self.results.get(id) {
                            invalidate.push(*seq);
                        }
                    }
                }
                for seq in invalidate {
                    if let Ok(i) = self.items.binary_search_by_key(&(seq, 0), Item::key) {
                        self.rendered[i] = None;
                    }
                }
            }
            EntryBody::ToolResult { call_id, .. } => {
                self.results.insert(call_id.clone(), entry.seq);
            }
            _ => {}
        }
        self.items.insert(pos, Item::Entry(entry));
        self.rendered.insert(pos, None);
        self.derived_stale = true;
        true
    }

    /// Drop everything after entry `seq` (a rewind elsewhere).
    pub fn truncate_after(&mut self, seq: u64) {
        let keep: Vec<bool> = self.items.iter().map(|i| i.key().0 <= seq).collect();
        let mut k = keep.iter();
        self.items.retain(|_| *k.next().unwrap_or(&false));
        let mut k = keep.iter();
        self.rendered.retain(|_| *k.next().unwrap_or(&false));
        self.results.retain(|_, s| *s <= seq);
        self.calls = self
            .entries()
            .flat_map(|e| match &e.body {
                EntryBody::Assistant { content, .. } => content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::ToolCall { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        // Tool results whose call went away render differently now.
        for r in self.rendered.iter_mut() {
            *r = None;
        }
        self.derived_stale = true;
    }

    /// Swap in an entry the server rewrote in place (a secret moved out of
    /// the conversation). Returns the one it replaced.
    pub fn replace_entry(&mut self, entry: Entry) -> Option<Entry> {
        let i = self
            .items
            .binary_search_by_key(&(entry.seq, 0), Item::key)
            .ok()?;
        let Item::Entry(slot) = &mut self.items[i] else {
            return None;
        };
        let old = std::mem::replace(slot, entry);
        // Tool results show their call's arguments: redraw everything.
        for r in self.rendered.iter_mut() {
            *r = None;
        }
        self.derived_stale = true;
        Some(old)
    }

    /// Your own messages, newest first: (seq, time, text).
    pub fn user_messages(&self) -> Vec<(u64, u64, String)> {
        self.entries()
            .rev()
            .filter_map(|e| match &e.body {
                EntryBody::User {
                    content,
                    author: None,
                } => Some((e.seq, e.ts, plain_text(content))),
                _ => None,
            })
            .filter(|(_, _, t)| !t.trim().is_empty())
            .collect()
    }

    pub fn add_local(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        // A model retry replaces the one before it: one line that counts
        // up ("attempt 2/5"), not a pile of warnings.
        let last = self.last_seq();
        if is_retry(&text)
            && let Some(Item::Local {
                after,
                level: l,
                text: prev,
                ..
            }) = self.items.last_mut()
            && *after == last
            && is_retry(prev)
        {
            *prev = text;
            *l = level;
            if let Some(r) = self.rendered.last_mut() {
                *r = None;
            }
            return;
        }
        self.local_n += 1;
        let item = Item::Local {
            after: last,
            n: self.local_n,
            level,
            text,
        };
        let pos = self
            .items
            .binary_search_by_key(&item.key(), Item::key)
            .unwrap_or_else(|p| p);
        self.items.insert(pos, item);
        self.rendered.insert(pos, None);
    }

    pub fn user_texts(&self) -> Vec<String> {
        self.entries()
            .filter_map(|e| match &e.body {
                EntryBody::User {
                    content,
                    author: None,
                } => Some(plain_text(content)),
                _ => None,
            })
            .filter(|t| !t.trim().is_empty())
            .collect()
    }

    /// Items that fold away, as in the apps: a completion check the agent
    /// answered with only ledger updates and a bare ✓ (its answer above
    /// stands), with that answer.
    fn quiet_checks(&self) -> Vec<bool> {
        let mut hidden: Vec<bool> = self.items.iter().map(|it|
            matches!(it, Item::Entry(e) if tessera_core::tools::meta::is_protocol_entry(e))).collect();
        let mut i = 0;
        while i < self.items.len() {
            let Item::Entry(e) = &self.items[i] else {
                i += 1;
                continue;
            };
            if !is_completion_check(e) {
                i += 1;
                continue;
            }
            let mut end = i + 1;
            let mut quiet = true;
            while let Some(Item::Entry(e)) = self.items.get(end) {
                match quiet_reply(e) {
                    Some(true) => end += 1,
                    Some(false) => {
                        quiet = false;
                        break;
                    }
                    None => break,
                }
            }
            if quiet {
                hidden[i..end].iter_mut().for_each(|h| *h = true);
                i = end;
            } else {
                i += 1;
            }
        }
        hidden
    }

    /// The agent is answering a completion check, quietly so far: what it
    /// streams (a ✓, ledger updates) isn't shown.
    pub fn quiet_check_open(&self) -> bool {
        for it in self.items.iter().rev() {
            let Item::Entry(e) = it else { return false };
            if is_completion_check(e) {
                return true;
            }
            if quiet_reply(e) != Some(true) {
                return false;
            }
        }
        false
    }

    pub fn last_assistant_text(&self) -> Option<String> {
        let hidden = self.quiet_checks();
        let shown = self
            .items
            .iter()
            .zip(hidden)
            .filter_map(|(it, h)| match it {
                Item::Entry(e) if !h => Some(e),
                _ => None,
            });
        shown.rev().find_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => {
                let t: Vec<&str> = content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text }
                            if !tessera_core::tools::meta::is_check_ack(text) =>
                        {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                let t = t.join("\n\n");
                (!t.trim().is_empty()).then_some(t)
            }
            _ => None,
        })
    }

    fn call_state<'a>(&'a self, id: &str, env: &'a Env<'_>) -> CallState<'a> {
        if let Some(seq) = self.results.get(id)
            && let Some(Entry {
                body:
                    EntryBody::ToolResult {
                        content,
                        is_error,
                        details,
                        ..
                    },
                seq,
                ..
            }) = self.find(*seq)
        {
            return CallState::Done {
                content,
                is_error: *is_error,
                details: details.as_ref(),
                seq: *seq,
            };
        }
        if let Some(r) = env.running.get(id) {
            return CallState::Running {
                output: &r.output,
                elapsed: env.now.saturating_duration_since(r.started),
            };
        }
        if env.approvals.iter().any(|a| a.call_id == id) {
            return CallState::Approval;
        }
        if env.active {
            CallState::Pending
        } else {
            CallState::NotRun
        }
    }

    /// Observed durations and which subagents reported failure.
    fn derive(&mut self) {
        if !self.derived_stale {
            return;
        }
        self.derived_stale = false;
        let mut durations = HashMap::new();
        let mut reports = HashMap::new();
        for e in self.entries() {
            match &e.body {
                EntryBody::ToolResult {
                    call_id, details, ..
                } => {
                    if let Some(ms) = details
                        .as_ref()
                        .and_then(|d| d.get("duration_ms"))
                        .and_then(Value::as_u64)
                    {
                        durations.insert(call_id.clone(), ms);
                    }
                }
                EntryBody::User { content, author } => {
                    if let Some(child) = report_child(author.as_deref()) {
                        let r = parse_report(&plain_text(content));
                        reports.insert(child.to_string(), r.failed);
                    }
                }
                _ => {}
            }
        }
        self.durations = durations;
        self.reports = reports;
    }

    /// Which items fold into step and report groups, and which stack
    /// tightly under the one before.
    fn plan(&self, env: &Env<'_>) -> Vec<Plan> {
        let mut plans = vec![Plan::default(); self.items.len()];
        let mut run = StepRun {
            min_tools: if env.channel { 1 } else { 2 },
            fold: !env.expand,
            ..StepRun::default()
        };
        let mut last_author: Option<String> = None;
        for (i, hidden) in self.quiet_checks().into_iter().enumerate() {
            plans[i].hidden = hidden;
        }
        for (i, item) in self.items.iter().enumerate() {
            if plans[i].hidden {
                continue;
            }
            let e = match item {
                Item::Entry(e) => e,
                Item::Local { .. } => {
                    run.flush(&mut plans, true);
                    continue;
                }
            };
            match &e.body {
                EntryBody::ToolResult { call_id, .. } if self.calls.contains(call_id) => {}
                EntryBody::Assistant {
                    content,
                    author,
                    error,
                    ..
                } => {
                    let who = author.as_deref().or(env.bot).map(str::to_string);
                    if env.channel && who != last_author {
                        run.flush(&mut plans, true);
                    }
                    last_author = who;
                    let calls_tools = content
                        .iter()
                        .any(|p| matches!(p, AssistantPart::ToolCall { .. }));
                    let mut has_steps = false;
                    let mut breaks = error.is_some();
                    for p in content {
                        match p {
                            AssistantPart::Thinking { text, .. } if !text.trim().is_empty() => {
                                has_steps = true;
                                run.states.push(Sq::Thought);
                                run.add_ms(env.think_ms.get(&e.seq).copied());
                            }
                            AssistantPart::Text { text } if !text.trim().is_empty() => {
                                if calls_tools {
                                    has_steps = true;
                                } else {
                                    breaks = true;
                                }
                            }
                            AssistantPart::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                has_steps = true;
                                let p = ToolPresentation::for_call(name, arguments);
                                run.housekeeping |= p.housekeeping;
                                let state = self.call_state(id, env);
                                let quiet = p.housekeeping
                                    && matches!(
                                        state,
                                        CallState::Done {
                                            is_error: false,
                                            ..
                                        }
                                    )
                                    && state.square() == Sq::Done;
                                if !quiet {
                                    run.states.push(state.square());
                                    run.tools.push(p.kind);
                                    run.add_ms(self.durations.get(id).copied());
                                }
                            }
                            _ => {}
                        }
                    }
                    if has_steps {
                        run.items.push(i);
                    }
                    if breaks {
                        run.flush(&mut plans, true);
                    }
                }
                _ => run.flush(&mut plans, true),
            }
        }
        // While the agent works, its latest steps stay open.
        run.flush(&mut plans, !env.active);
        // Reports: three or more in a row fold into one row.
        let mut reps: Vec<(usize, Sq)> = Vec::new();
        let fold_reports = |plans: &mut Vec<Plan>, reps: &mut Vec<(usize, Sq)>| {
            if reps.len() >= 3 && !env.expand {
                for (i, _) in reps.iter() {
                    plans[*i].report_fold = true;
                }
                plans[reps[0].0].report_head = Some(ReportsRow {
                    states: reps.iter().map(|r| r.1).collect(),
                });
            }
            reps.clear();
        };
        for (i, item) in self.items.iter().enumerate() {
            if plans[i].hidden {
                continue;
            }
            match item {
                Item::Entry(Entry {
                    body: EntryBody::User { author, .. },
                    ..
                }) if report_child(author.as_deref()).is_some() => {
                    let child = report_child(author.as_deref()).unwrap_or_default();
                    let failed = self.reports.get(child).copied().unwrap_or(false);
                    reps.push((i, if failed { Sq::Failed } else { Sq::Done }));
                }
                Item::Entry(Entry {
                    body: EntryBody::ToolResult { call_id, .. },
                    ..
                }) if self.calls.contains(call_id) => {}
                _ => fold_reports(&mut plans, &mut reps),
            }
        }
        fold_reports(&mut plans, &mut reps);
        // Spacing: an item that starts with a row stacks under one that
        // ended with a row.
        let mut prev_end: Option<Kind> = None;
        for (i, item) in self.items.iter().enumerate() {
            let Some((first, last)) = self.edges(item, &plans[i], env) else {
                continue;
            };
            plans[i].tight = first == Kind::Row && prev_end == Some(Kind::Row);
            prev_end = Some(last);
        }
        plans
    }

    /// The shapes of an item's first and last blocks (None: draws nothing).
    fn edges(&self, item: &Item, plan: &Plan, env: &Env<'_>) -> Option<(Kind, Kind)> {
        if plan.hidden {
            return None;
        }
        let Item::Entry(e) = item else {
            return Some((Kind::Prose, Kind::Prose));
        };
        match &e.body {
            EntryBody::ToolResult { call_id, .. } if self.calls.contains(call_id) => None,
            EntryBody::ToolResult { .. } => Some((Kind::Row, Kind::Row)),
            EntryBody::User { .. } if plan.report_fold && plan.report_head.is_none() => None,
            EntryBody::Assistant { content, error, .. } => {
                let calls_tools = content
                    .iter()
                    .any(|p| matches!(p, AssistantPart::ToolCall { .. }));
                let mut kinds: Vec<Kind> = Vec::new();
                if plan.head.is_some() {
                    kinds.push(Kind::Row);
                }
                for p in content {
                    let is_step = match p {
                        AssistantPart::Thinking { text, .. } => {
                            if text.trim().is_empty() {
                                continue;
                            }
                            true
                        }
                        AssistantPart::Text { text } => {
                            if text.trim().is_empty() {
                                continue;
                            }
                            calls_tools
                        }
                        AssistantPart::ToolCall { .. } => true,
                    };
                    if plan.fold && is_step {
                        continue;
                    }
                    kinds.push(match p {
                        AssistantPart::Thinking { .. } if !env.thinking => Kind::Row,
                        AssistantPart::ToolCall { .. } => Kind::Row,
                        _ => Kind::Prose,
                    });
                }
                if error.is_some() {
                    kinds.push(Kind::Prose);
                }
                Some((*kinds.first()?, *kinds.last()?))
            }
            _ => Some((Kind::Prose, Kind::Prose)),
        }
    }

    fn item_key(&self, item: &Item, env: &Env<'_>, header: Option<&Speaker>, plan: &Plan) -> u64 {
        let mut h = DefaultHasher::new();
        (env.width, env.expand, env.thinking, env.theme.cache_key()).hash(&mut h);
        plan.hash(&mut h);
        if let Some(sp) = header {
            sp.hash(&mut h);
            if let Speaker::Bot(id) = sp
                && let Some(b) = env.bots.get(id)
            {
                (&b.name, &b.color, &b.handle).hash(&mut h);
            }
        }
        if env.channel {
            1u8.hash(&mut h);
        }
        match item {
            Item::Entry(Entry {
                seq,
                body: EntryBody::Assistant { content, .. },
                ..
            }) => {
                env.think_ms.get(seq).hash(&mut h);
                for p in content {
                    if let AssistantPart::ToolCall { id, name, .. } = p {
                        self.durations.get(id).hash(&mut h);
                        match self.call_state(id, env) {
                            CallState::Done { seq, .. } => (1u8, seq).hash(&mut h),
                            CallState::Running { output, elapsed } => {
                                (2u8, output.len(), elapsed.as_secs()).hash(&mut h);
                            }
                            CallState::Approval => 3u8.hash(&mut h),
                            CallState::Pending => 4u8.hash(&mut h),
                            CallState::NotRun => (5u8, env.interrupted).hash(&mut h),
                            CallState::Streaming => 6u8.hash(&mut h),
                        }
                        if name == "agent" {
                            for c in env.children {
                                (&c.id, c.status as u8, &c.preview, &c.title, c.steps).hash(&mut h);
                            }
                            self.reports.len().hash(&mut h);
                        }
                    }
                }
            }
            Item::Entry(Entry {
                body: EntryBody::ToolResult { call_id, .. },
                ..
            }) => self.calls.contains(call_id).hash(&mut h),
            _ => {}
        }
        h.finish()
    }

    /// Bring every item's cached lines up to date; returns the total height.
    pub fn prepare(&mut self, env: &Env<'_>) -> usize {
        self.derive();
        let plans = self.plan(env);
        let mut total = 0;
        let labelled = env.channel || env.bot.is_some();
        let mut prev: Option<Speaker> = None;
        for (i, plan) in plans.iter().enumerate() {
            let sp = speaker_of(&self.items[i], env);
            // Words from another chat's agent or a scheduled job say so
            // everywhere (a subagent's task, a job's instructions).
            let named = |s: &Speaker| {
                labelled
                    || matches!(s, Speaker::Bot(id) if id.starts_with("chat:") || id.starts_with("job:"))
            };
            let header = match &sp {
                Some(s) if named(s) && prev.as_ref() != Some(s) && *s != Speaker::You => {
                    Some(s.clone())
                }
                _ => None,
            };
            if sp.is_some() {
                prev = sp;
            }
            let key = self.item_key(&self.items[i], env, header.as_ref(), plan);
            let stale = self.rendered[i].as_ref().is_none_or(|r| r.key != key);
            if stale {
                #[cfg(test)]
                {
                    self.renders += 1;
                }
                let lines = self.render_item(&self.items[i], env, header.as_ref(), plan);
                self.rendered[i] = Some(Rendered { key, lines });
            }
            total += self.rendered[i]
                .as_ref()
                .map(|r| r.lines.len())
                .unwrap_or(0);
        }
        self.last_speaker = prev;
        total
    }

    /// Speaker of the most recent message (valid after `prepare`).
    pub fn last_speaker(&self) -> Option<&Speaker> {
        self.last_speaker.as_ref()
    }

    /// Whether the last rendered block is a row (for spacing what follows).
    pub fn ends_with_row(&self) -> bool {
        self.items
            .iter()
            .rev()
            .find_map(|it| match it {
                Item::Entry(Entry {
                    body: EntryBody::ToolResult { call_id, .. },
                    ..
                }) if self.calls.contains(call_id) => None,
                Item::Entry(Entry {
                    body: EntryBody::Assistant { content, .. },
                    ..
                }) => Some(matches!(
                    content.last(),
                    Some(AssistantPart::ToolCall { .. })
                )),
                _ => Some(false),
            })
            .unwrap_or(false)
    }

    /// Cached lines, in order (call `prepare` first).
    pub fn blocks(&self) -> impl Iterator<Item = &[Line<'static>]> {
        self.rendered
            .iter()
            .map(|r| r.as_ref().map(|r| r.lines.as_slice()).unwrap_or(&[]))
    }

    fn render_item(
        &self,
        item: &Item,
        env: &Env<'_>,
        header: Option<&Speaker>,
        plan: &Plan,
    ) -> Vec<Line<'static>> {
        if plan.hidden {
            return Vec::new();
        }
        let mut body = match item {
            Item::Local { level, text, .. } => notice(*level, text, env),
            Item::Entry(e) => match &e.body {
                EntryBody::User { content, author } => {
                    let author = author.as_deref();
                    if report_child(author).is_some() {
                        if let Some(r) = &plan.report_head {
                            vec![reports_row(r, env)]
                        } else if plan.report_fold {
                            Vec::new()
                        } else {
                            report_card(&plain_text(content), env)
                        }
                    } else if author == Some("goal") {
                        goal_note(&plain_text(content), env)
                    } else if author == Some("harness") {
                        harness_note(&plain_text(content), env)
                    } else if author.is_some() || is_dm(env) {
                        let clean = text::sanitize(content_text(content).trim_end()).into_owned();
                        let pre = [span("  ", Style::new())];
                        text::wrap_plain(&clean, env.theme.text(), env.width, &pre, &pre)
                    } else {
                        user_block(content, env)
                    }
                }
                EntryBody::Assistant {
                    content,
                    error,
                    stop,
                    ..
                } => self.assistant_block(content, error.as_deref(), *stop, e.seq, e.ts, plan, env),
                EntryBody::ToolResult {
                    call_id,
                    tool,
                    content,
                    is_error,
                    details,
                    ..
                } => {
                    if self.calls.contains(call_id) {
                        Vec::new()
                    } else {
                        let state = CallState::Done {
                            content,
                            is_error: *is_error,
                            details: details.as_ref(),
                            seq: e.seq,
                        };
                        tool_card(tool, &Value::Null, state, 0, None, &self.reports, env)
                    }
                }
                EntryBody::Compaction {
                    summary,
                    tokens_before,
                    tokens_after,
                    ..
                } => compaction(summary, *tokens_before, *tokens_after, env),
                EntryBody::Notice {
                    level,
                    text,
                    learned,
                } => {
                    if !learned.is_empty() || text.starts_with(LEARNED_PREFIX) {
                        learned_block(text, learned, env)
                    } else {
                        notice(*level, text, env)
                    }
                }
            },
        };
        if let Some(sp) = header
            && !body.is_empty()
        {
            body.insert(0, speaker_line(sp, env, None));
        }
        if plan.tight || body.is_empty() {
            body
        } else {
            with_gap(body)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assistant_block(
        &self,
        content: &[AssistantPart],
        error: Option<&str>,
        stop: StopReason,
        seq: u64,
        ts: u64,
        plan: &Plan,
        env: &Env<'_>,
    ) -> Vec<Line<'static>> {
        let mut blocks: Vec<(Kind, Vec<Line<'static>>)> = Vec::new();
        if let Some(g) = &plan.head {
            blocks.push((Kind::Row, vec![steps_row(g, env)]));
        }
        let calls_tools = content
            .iter()
            .any(|p| matches!(p, AssistantPart::ToolCall { .. }));
        for p in content {
            let is_step = !matches!(p, AssistantPart::Text { .. }) || calls_tools;
            if plan.fold && is_step {
                continue;
            }
            match p {
                AssistantPart::Thinking { text, .. } => {
                    if !text.trim().is_empty() {
                        let ms = env.think_ms.get(&seq).copied();
                        blocks.push(thinking_block(text, None, ms, env));
                    }
                }
                AssistantPart::Text { text } => {
                    if !text.trim().is_empty() {
                        blocks.push((Kind::Prose, answer_block(text, env)));
                    }
                }
                AssistantPart::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    let state = self.call_state(id, env);
                    let dur = self.durations.get(id).copied();
                    blocks.push((
                        Kind::Row,
                        tool_card(name, arguments, state, ts, dur, &self.reports, env),
                    ));
                }
            }
        }
        if let Some(e) = error {
            blocks.push((Kind::Prose, notice(NoticeLevel::Error, e, env)));
        }
        if blocks.is_empty() && stop == StopReason::Aborted {
            blocks.push((
                Kind::Prose,
                vec![Line::from(span("  (interrupted)", env.theme.dim()))],
            ));
        }
        join_blocks(blocks)
    }
}

/// A private conversation with a bot (a channel with one bot of its own).
fn is_dm(env: &Env<'_>) -> bool {
    env.channel && env.bot.is_some()
}

fn speaker_of(item: &Item, env: &Env<'_>) -> Option<Speaker> {
    let Item::Entry(e) = item else { return None };
    match &e.body {
        EntryBody::User { author, .. } if is_system_author(author.as_deref()) => None,
        EntryBody::User {
            author: Some(a), ..
        } => Some(Speaker::Bot(a.clone())),
        // Nobody types into a bot DM: an older server's message there
        // without an author came from one of the user's chats.
        EntryBody::User { .. } if is_dm(env) => Some(Speaker::Bot("chat:".into())),
        EntryBody::User { .. } => Some(Speaker::You),
        EntryBody::Assistant { author, .. } => Some(match author.as_deref().or(env.bot) {
            Some(b) => Speaker::Bot(b.to_string()),
            None => Speaker::Agent,
        }),
        _ => None,
    }
}

/// Leading blank line separates non-empty blocks.
fn with_gap(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if !lines.is_empty() {
        lines.insert(0, Line::default());
    }
    lines
}

/// Rows stack; anything next to prose gets a blank line.
fn join_blocks(blocks: Vec<(Kind, Vec<Line<'static>>)>) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut prev: Option<Kind> = None;
    for (kind, b) in blocks {
        if b.is_empty() {
            continue;
        }
        if let Some(p) = prev
            && !(p == Kind::Row && kind == Kind::Row)
        {
            out.push(Line::default());
        }
        out.extend(b);
        prev = Some(kind);
    }
    out
}

fn indent(lines: Vec<Line<'static>>, n: usize) -> Vec<Line<'static>> {
    let pad = " ".repeat(n);
    lines
        .into_iter()
        .map(|mut l| {
            if !l.spans.is_empty() {
                l.spans.insert(0, Span::raw(pad.clone()));
            }
            l
        })
        .collect()
}

// ---- tool cards ---------------------------------------------------------------

fn first_line(s: &str) -> (String, bool) {
    let mut lines = s.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("").to_string();
    (first, lines.next().is_some())
}

/// Read one string field out of possibly incomplete JSON.
fn partial_str(raw: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = raw.find(&pat)? + pat.len();
    let rest = raw[i..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => {}
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(o) => out.push(o),
                None => break,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

fn primary_key(name: &str) -> &'static str {
    match name {
        "bash" => "command",
        "read" | "write" | "edit" => "path",
        "grep" | "glob" => "pattern",
        "web_fetch" => "url",
        "web_search" | "recall" => "query",
        "task" => "goal",
        "memory" => "content",
        "skill_view" | "skill_manage" | "schedule" => "name",
        "job" => "action",
        "whiteboard" => "title",
        "browser" => "action",
        _ => "",
    }
}

/// `https://www.example.com/a/b?x` → `example.com/a/b`.
fn short_url(u: &str) -> String {
    let rest = u
        .trim()
        .strip_prefix("https://")
        .or_else(|| u.trim().strip_prefix("http://"))
        .unwrap_or(u.trim());
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    rest.trim_end_matches('/').to_string()
}

/// What a `browser` call does, in a few words.
fn browser_summary(args: &Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let target = || {
        if !s("ref").is_empty() {
            format!("@{}", s("ref"))
        } else if !s("text").is_empty() {
            format!("“{}”", first_line(&s("text")).0)
        } else {
            match (
                args.get("x").and_then(Value::as_f64),
                args.get("y").and_then(Value::as_f64),
            ) {
                (Some(x), Some(y)) => format!("{x:.0},{y:.0}"),
                _ => String::new(),
            }
        }
    };
    let action = s("action");
    let detail = match action.as_str() {
        "open" | "navigate" | "go" => {
            let mut d = short_url(&s("url"));
            if args.get("show").and_then(Value::as_bool) == Some(true) {
                d.push_str("  (beside)");
            }
            d
        }
        "click" => target(),
        "type" => {
            let text = first_line(&s("text")).0;
            let into = if s("ref").is_empty() {
                String::new()
            } else {
                format!(" into @{}", s("ref"))
            };
            let enter = if args.get("submit").and_then(Value::as_bool) == Some(true) {
                " ⏎"
            } else {
                ""
            };
            format!("“{}”{into}{enter}", text::truncate(&text, 40))
        }
        "press" => s("key"),
        "scroll" => [s("direction"), s("ref")]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        "select" => {
            let v = [s("value"), s("option"), s("label")]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or_default();
            if s("ref").is_empty() {
                v
            } else {
                format!("{v} in @{}", s("ref"))
            }
        }
        "wait" => {
            if !s("text").is_empty() {
                format!("for “{}”", first_line(&s("text")).0)
            } else if let Some(ms) = args.get("ms").and_then(Value::as_u64) {
                fmt_ms(ms)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };
    let action = if action.is_empty() {
        "open".to_string()
    } else {
        action
    };
    if detail.is_empty() {
        action
    } else {
        format!("{action} · {detail}")
    }
}

/// Path relative to `cwd` when inside it.
fn rel(path: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    if cwd.is_empty() {
        return path.to_string();
    }
    if path == cwd {
        return ".".into();
    }
    match path.strip_prefix(cwd).and_then(|r| r.strip_prefix('/')) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => path.to_string(),
    }
}

/// Drop a leading `cd <cwd> &&` (a no-op the model likes to add).
fn strip_cd(cmd: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    if cwd.is_empty() {
        return cmd.to_string();
    }
    for q in ["", "\"", "'"] {
        for sep in [" && ", "; ", " ;", "&&"] {
            let pre = format!("cd {q}{cwd}{q}{sep}");
            if let Some(rest) = cmd.strip_prefix(&pre) {
                return rest.trim_start().to_string();
            }
        }
    }
    cmd.to_string()
}

/// One-line human summary of a tool call's arguments. Paths under `cwd` are
/// shown relative to it.
pub fn tool_summary(name: &str, args: &Value, cwd: &str) -> String {
    let p = ToolPresentation::for_call(name, args);
    let name = p.kind.as_str();
    let args = &p.arguments;
    let parsed;
    let args = match args {
        Value::String(raw) => {
            parsed = serde_json::from_str::<Value>(raw).ok();
            match &parsed {
                Some(v) if v.is_object() => v,
                _ => {
                    let key = primary_key(name);
                    let v = if key.is_empty() {
                        None
                    } else {
                        partial_str(raw, key).or_else(|| match key {
                            "path" => partial_str(raw, "file_path")
                                .or_else(|| partial_str(raw, "filePath")),
                            "command" => {
                                partial_str(raw, "cmd").or_else(|| partial_str(raw, "script"))
                            }
                            _ => None,
                        })
                    };
                    let v = v.map(|v| first_line(&v).0).unwrap_or_default();
                    return match key {
                        "path" => rel(&v, cwd),
                        "command" => strip_cd(&v, cwd),
                        _ => v,
                    };
                }
            }
        }
        v => v,
    };
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let n = |k: &str| {
        args.get(k).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    };
    match name {
        "bash" => {
            let (first, more) = first_line(&strip_cd(&s("command"), cwd));
            let bg = args
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            format!(
                "{first}{}{}",
                if more { " …" } else { "" },
                if bg { "  (background)" } else { "" }
            )
        }
        "read" => {
            let mut p = rel(&s("path"), cwd);
            // Several files: all of them.
            if let Some(more) = args.get("paths").and_then(Value::as_array) {
                for f in more.iter().filter_map(Value::as_str) {
                    let f = rel(f, cwd);
                    if f != p && !p.split(", ").any(|x| x == f) {
                        if !p.is_empty() {
                            p.push_str(", ");
                        }
                        p.push_str(&f);
                    }
                }
            }
            match (n("offset"), n("limit")) {
                (Some(o), Some(l)) => p.push_str(&format!(":{o}-{}", o + l.saturating_sub(1))),
                (Some(o), None) => p.push_str(&format!(":{o}-")),
                (None, Some(l)) => p.push_str(&format!(":1-{l}")),
                _ => {}
            }
            p
        }
        "write" | "edit" => rel(&s("path"), cwd),
        "grep" => {
            let mut out = s("pattern");
            if !s("path").is_empty() {
                out.push_str(&format!("  in {}", rel(&s("path"), cwd)));
            }
            if !s("glob").is_empty() {
                out.push_str(&format!("  ({})", s("glob")));
            }
            out
        }
        "glob" => {
            let mut out = s("pattern");
            if !s("path").is_empty() {
                out.push_str(&format!("  in {}", rel(&s("path"), cwd)));
            }
            out
        }
        "web_fetch" => s("url"),
        "web_search" => s("query"),
        "task" => {
            if !s("goal").is_empty() {
                first_line(&s("goal")).0
            } else if let Some(u) = args.get("update").and_then(Value::as_array) {
                format!(
                    "{} plan update{}",
                    u.len(),
                    if u.len() == 1 { "" } else { "s" }
                )
            } else if let Some(p) = args.get("plan").and_then(Value::as_array) {
                format!(
                    "plan with {} step{}",
                    p.len(),
                    if p.len() == 1 { "" } else { "s" }
                )
            } else {
                "ledger update".into()
            }
        }
        "memory" => {
            let what = if s("content").is_empty() {
                s("old")
            } else {
                s("content")
            };
            format!(
                "{} {}: {}",
                s("action"),
                if s("target").is_empty() {
                    "memory".into()
                } else {
                    s("target")
                },
                first_line(&what).0
            )
        }
        "skill_view" => {
            if s("file").is_empty() {
                s("name")
            } else {
                format!("{}/{}", s("name"), s("file"))
            }
        }
        "skill_manage" | "schedule" => {
            let target = if s("name").is_empty() {
                s("id")
            } else {
                s("name")
            };
            format!("{} {}", s("action"), target).trim().to_string()
        }
        "job" => format!("{} {}", s("action"), s("id")).trim().to_string(),
        "whiteboard" => {
            let what = [s("title"), s("prompt"), s("id")]
                .into_iter()
                .find(|x| !x.is_empty())
                .unwrap_or_default();
            let action = if s("action").is_empty() {
                "show".to_string()
            } else {
                s("action")
            };
            if what.is_empty() {
                action
            } else {
                format!("{action} · {}", first_line(&what).0)
            }
        }
        "browser" => browser_summary(args),
        "recall" => {
            if !s("query").is_empty() {
                let mut q = format!("“{}”", s("query"));
                if s("scope") == "all" {
                    q.push_str("  in all chats");
                }
                if !s("since").is_empty() {
                    q.push_str(&format!("  since {}", s("since")));
                }
                q
            } else if let Some(seq) = n("seq") {
                format!("entry #{seq}")
            } else if !s("artifact").is_empty() {
                format!("artifact {}", s("artifact"))
            } else {
                String::new()
            }
        }
        "agent" => {
            let tasks = args
                .get("tasks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let names: Vec<String> = tasks
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
                .collect();
            let head = format!(
                "{} subagent{}",
                tasks.len(),
                if tasks.len() == 1 { "" } else { "s" }
            );
            if names.is_empty() {
                head
            } else {
                format!("{head}: {}", names.join(", "))
            }
        }
        _ => args
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(|s| first_line(s).0)))
            .unwrap_or_default(),
    }
}

fn content_text(content: &[ContentPart]) -> String {
    let mut out = String::new();
    for p in content {
        if !out.is_empty() {
            out.push('\n');
        }
        match p {
            ContentPart::Text { text } => out.push_str(text),
            ContentPart::Image { mime, .. } => out.push_str(&format!("[image {mime}]")),
        }
    }
    out
}

/// Split a trailing `[exit 0 · 1.2s]` status line off bash output.
fn bash_status(text_s: &str) -> (&str, Option<&str>) {
    let trimmed = text_s.trim_end();
    let (body, last) = match trimmed.rfind('\n') {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => ("", trimmed),
    };
    let status_of = |l: &str| -> Option<(usize, usize)> {
        let start = l.rfind('[')?;
        let inner = &l[start..];
        (inner.ends_with("s]") && inner.contains(" · ")).then_some((start, l.len()))
    };
    match status_of(last) {
        Some((s, e)) => {
            let prefix = last[..s].trim();
            if prefix == "(no output)" || prefix.is_empty() {
                let status = last[s + 1..e - 1].split(" · ").next().unwrap_or("");
                (
                    body,
                    Some(if status == "killed" {
                        "Stopped"
                    } else {
                        status
                    }),
                )
            } else {
                (trimmed, None)
            }
        }
        None => (trimmed, None),
    }
}

// ---- blocks ------------------------------------------------------------------------

/// A line: `left` then `right` pushed to the right edge of `width`
/// (`left` is shortened to make room).
fn row(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rw: usize = right.iter().map(|s| text::width(&s.content)).sum();
    if right.is_empty() || rw + 4 > width {
        return text::truncate_line(Line::from(left), width);
    }
    let left = text::truncate_line(Line::from(left), width - rw - 2);
    let lw = text::line_width(&left);
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(lw + rw))));
    spans.extend(right);
    Line::from(spans)
}

/// "4s", "1m 20s", "2h 5m".
pub fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn fmt_elapsed(d: Duration) -> String {
    fmt_ms(d.as_millis() as u64)
}

/// The user's message: a gold bar and, where surfaces are painted, a
/// raised stone bubble fitted to the text.
pub fn user_block(content: &[ContentPart], env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let mut text_s = String::new();
    let mut images = 0;
    for p in content {
        match p {
            ContentPart::Text { text } => {
                if !text_s.is_empty() {
                    text_s.push('\n');
                }
                text_s.push_str(text);
            }
            ContentPart::Image { .. } => images += 1,
        }
    }
    let clean = text::sanitize(text_s.trim_end()).into_owned();
    let surf = t.surfaces();
    let bubble = t.bg(Tok::Raised);
    // Bar, one cell of padding, text, one cell of padding.
    let inner = env.width.saturating_sub(4).max(8);
    let mut body = if clean.trim().is_empty() {
        Vec::new()
    } else {
        text::wrap_plain(&clean, t.user().patch(bubble), inner, &[], &[])
    };
    if images > 0 {
        body.push(Line::from(span(
            format!("▣ {images} image{}", if images == 1 { "" } else { "s" }),
            t.dim().patch(bubble),
        )));
    }
    let widest = body.iter().map(text::line_width).max().unwrap_or(0);
    let bar = span("▎", t.accent().patch(bubble));
    body.into_iter()
        .map(|l| {
            let w = text::line_width(&l);
            let mut spans = vec![bar.clone(), span(" ", bubble)];
            spans.extend(l.spans);
            if surf {
                spans.push(span(" ".repeat(widest - w + 1), bubble));
            }
            Line::from(spans)
        })
        .collect()
}

pub fn answer_block(md: &str, env: &Env<'_>) -> Vec<Line<'static>> {
    indent(
        markdown::render(md, env.width.saturating_sub(2), env.theme),
        2,
    )
}

/// The last line of reasoning, as plain text (the live preview).
fn last_thought(s: &str) -> String {
    let tail = &s[tessera_core::util::floor_char(s, s.len().saturating_sub(2400))..];
    tail.lines()
        .rev()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '>', '-', '*', ' '])
                .replace("**", "")
                .replace('`', "")
        })
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

/// Reasoning as one quiet row: "Thinking · 8s" with the working indicator
/// and a one-line preview while it streams, "Thought for 12s" after.
/// Shown in full with ctrl+t.
fn thinking_block(
    text_s: &str,
    live: Option<Instant>,
    ms: Option<u64>,
    env: &Env<'_>,
) -> (Kind, Vec<Line<'static>>) {
    let t = env.theme;
    let mut head: Vec<Span<'static>> = Vec::new();
    match live {
        Some(started) => {
            head.extend(env.spinner());
            let secs = env.now.saturating_duration_since(started).as_secs();
            let label = if secs >= 1 {
                format!(" Thinking · {secs}s")
            } else {
                " Thinking".to_string()
            };
            head.push(span(label, t.muted()));
            if !env.thinking {
                let preview = text::sanitize(&last_thought(text_s)).into_owned();
                if !preview.is_empty() {
                    head.push(span(format!("  {preview}"), t.faint()));
                }
            }
        }
        None => {
            head.push(span("◇", t.faint()));
            let label = match ms {
                Some(ms) if ms > 500 => format!(" Thought for {}", fmt_ms(ms.max(1000))),
                _ => " Thought process".to_string(),
            };
            head.push(span(label, t.dim()));
        }
    }
    let mut out = vec![text::truncate_line(Line::from(head), env.width)];
    if !env.thinking {
        return (Kind::Row, out);
    }
    let clean = text::sanitize(text_s.trim());
    let pre = [span("│ ", t.hairline())];
    out.extend(text::wrap_plain(
        &clean,
        t.thinking(),
        env.width,
        &pre,
        &pre,
    ));
    (Kind::Prose, out)
}

/// The streaming message's reasoning (the pane measures its time).
pub fn live_thinking(
    text_s: &str,
    started: Option<Instant>,
    ms: Option<u64>,
    env: &Env<'_>,
) -> Vec<Line<'static>> {
    thinking_block(text_s, started, ms, env).1
}

pub fn notice(level: NoticeLevel, msg: &str, env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let (glyph, gs, style) = match level {
        NoticeLevel::Info => ("▪ ", t.faint(), t.dim()),
        NoticeLevel::Warning => ("■ ", t.warning(), t.warning()),
        NoticeLevel::Error => ("■ ", t.error(), t.error()),
    };
    let clean = text::sanitize(msg.trim());
    let first = [span("  ", Style::new()), span(glyph, gs)];
    let cont = [span("    ", Style::new())];
    text::wrap_plain(&clean, style, env.width, &first, &cont)
}

const LEARNED_PREFIX: &str = "Learned from this conversation";

/// Older servers only wrote the text: "Learned from this conversation —
/// about you: …; note: …".
fn parse_learned(text_s: &str) -> Vec<LearnedItem> {
    let body = text_s
        .strip_prefix(LEARNED_PREFIX)
        .unwrap_or(text_s)
        .trim_start_matches([' ', '—', '-', ':'])
        .trim();
    body.split("; ")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let (kind, rest) = match p.split_once(": ") {
                Some(("about you", r)) => (LearnedKind::Profile, r),
                Some(("note", r)) => (LearnedKind::Note, r),
                Some(("skill", r)) => (LearnedKind::Skill, r),
                _ => (LearnedKind::Note, p),
            };
            LearnedItem {
                kind,
                action: LearnedAction::Added,
                text: rest.trim().to_string(),
            }
        })
        .collect()
}

/// What a memory review saved: one calm line, a tidy list with ctrl+o.
fn learned_block(text_s: &str, items: &[LearnedItem], env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let parsed;
    let items = if items.is_empty() {
        parsed = parse_learned(text_s);
        &parsed[..]
    } else {
        items
    };
    let n = items.len();
    let count = |a: LearnedAction| items.iter().filter(|i| i.action == a).count();
    let (added, updated, removed) = (
        count(LearnedAction::Added),
        count(LearnedAction::Updated),
        count(LearnedAction::Removed),
    );
    let things = |k: usize| format!("{k} thing{}", if k == 1 { "" } else { "s" });
    let label = if added == n {
        format!("Remembered {}", things(n))
    } else if removed == n {
        format!("Forgot {}", things(n))
    } else if updated == n {
        format!("Updated {}", things(n))
    } else {
        let mut parts = Vec::new();
        if added > 0 {
            parts.push(format!("{added} new"));
        }
        if updated > 0 {
            parts.push(format!("{updated} updated"));
        }
        if removed > 0 {
            parts.push(format!("{removed} forgotten"));
        }
        format!("Memory updated · {}", parts.join(", "))
    };
    let mut head = vec![span("  ◆ ", t.accent()), span(label, t.muted())];
    if !env.expand {
        head.push(span("  ctrl+o", t.faint()));
    }
    let mut out = vec![text::truncate_line(Line::from(head), env.width)];
    if !env.expand {
        return out;
    }
    const W: usize = 10;
    for it in items {
        let kind = match it.kind {
            LearnedKind::Profile => "About you",
            LearnedKind::Note => "Note",
            LearnedKind::Skill => "Skill",
        };
        let (style, suffix) = match it.action {
            LearnedAction::Added => (t.muted(), ""),
            LearnedAction::Updated => (t.muted(), "  updated"),
            LearnedAction::Removed => (t.dim().add_modifier(Modifier::CROSSED_OUT), ""),
        };
        let clean = text::sanitize(&tessera_core::util::one_line(&it.text, 400)).into_owned();
        let first = [
            span("    ", Style::new()),
            span(format!("{kind:<W$}"), t.faint()),
        ];
        let cont = [span(" ".repeat(4 + W), Style::new())];
        let mut lines = text::wrap_plain(&clean, style, env.width, &first, &cont);
        if !suffix.is_empty()
            && let Some(l) = lines.last_mut()
        {
            l.spans.push(span(suffix, t.faint()));
        }
        out.extend(lines);
    }
    out
}

/// "[The user set the goal for this session]" notes.
fn goal_note(text_s: &str, env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let goal = text_s
        .strip_prefix("[The user set the goal for this session]")
        .map(str::trim);
    let (label, body) = match goal {
        Some(g) => ("Goal", tessera_core::util::one_line(g, 400)),
        None => ("Goal cleared", String::new()),
    };
    let first = [
        span("  ◆ ", t.accent()),
        span(label, t.dim()),
        span(if body.is_empty() { "" } else { " · " }, t.faint()),
    ];
    let cont = [span("    ", Style::new())];
    text::wrap_plain(&text::sanitize(&body), t.muted(), env.width, &first, &cont)
}

/// How a tool result after a "no" to its approval starts (engine.rs,
/// tools/machines.rs, engine/claude_code.rs).
const DECLINED: &str = "The user declined this ";

/// What the harness told the agent (`[Budget reached] …`): one faint line,
/// its label and its first words. A completion check says how much of the
/// plan was still open.
fn harness_note(text_s: &str, env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let (label, body) = match text_s
        .trim_start()
        .strip_prefix('[')
        .and_then(|r| r.split_once(']'))
    {
        Some((l, b)) if !l.trim().is_empty() && l.len() <= 60 && !l.contains('\n') => {
            (l.trim(), b.trim())
        }
        _ => ("Note from Tessera", text_s.trim()),
    };
    let body = if label == "Completion check" {
        // Its open items (plan steps, "done when" criteria) are numbered
        // lines ("2. Run the tests [pending]").
        let open = body
            .lines()
            .filter(|l| {
                l.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .count();
        format!(
            "{open} item{} still open; the agent was asked to finish or confirm",
            if open == 1 { "" } else { "s" }
        )
    } else {
        tessera_core::util::one_line(body, 300)
    };
    let mut spans = vec![
        span("  ▪ ", t.faint()),
        span(text::sanitize(label).into_owned(), t.dim()),
    ];
    if !body.is_empty() {
        spans.push(span(" · ", t.faint()));
        spans.push(span(text::sanitize(&body).into_owned(), t.faint()));
    }
    vec![text::truncate_line(Line::from(spans), env.width)]
}

/// `[Report from subagent "name" · 7 steps · 2 min · session ses_…]`.
struct Report {
    name: String,
    meta: Vec<String>,
    body: String,
    failed: bool,
}

fn parse_report(text_s: &str) -> Report {
    let Some(rest) = text_s.strip_prefix("[Report from subagent \"") else {
        return Report {
            name: "Subagent".into(),
            meta: Vec::new(),
            failed: text_s.contains("The subagent did not finish"),
            body: text_s.to_string(),
        };
    };
    let (name, rest) = rest.split_once('"').unwrap_or(("Subagent", rest));
    let (meta, body) = rest.split_once(']').unwrap_or((rest, ""));
    let meta: Vec<String> = meta
        .split('·')
        .map(str::trim)
        .filter(|m| !m.is_empty() && !m.starts_with("session "))
        .map(str::to_string)
        .collect();
    let body = body.trim().to_string();
    Report {
        name: if name.is_empty() { "Subagent" } else { name }.to_string(),
        meta,
        failed: body.starts_with("The subagent did not finish"),
        body,
    }
}

/// A subagent's report, delivered into this chat.
fn report_card(text_s: &str, env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let r = parse_report(text_s);
    let mut head = vec![
        span("↳ ", t.faint()),
        span("Report from ", t.dim()),
        span(
            text::sanitize(&r.name).into_owned(),
            t.muted().add_modifier(Modifier::BOLD),
        ),
    ];
    if r.failed {
        head.push(span("  did not finish", t.error()));
    }
    let meta = vec![span(r.meta.join(" · "), t.faint())];
    let mut out = vec![row(head, meta, env.width)];
    let md = markdown::render(&r.body, env.width.saturating_sub(4), t);
    let limit = if env.expand { usize::MAX } else { 6 };
    let total = md.len();
    for l in md.into_iter().take(limit) {
        let mut spans = vec![span("  │ ", t.hairline())];
        spans.extend(l.spans);
        out.push(Line::from(spans));
    }
    if total > limit {
        out.push(Line::from(vec![
            span("  │ ", t.hairline()),
            span(
                format!("… {} more lines · ctrl+o", total - limit),
                t.faint(),
            ),
        ]));
    }
    out
}

/// Several reports in a row: "▪▪▪▪ Reports  24 from subagents".
fn reports_row(r: &ReportsRow, env: &Env<'_>) -> Line<'static> {
    let t = env.theme;
    let mut spans = motif::strip(t, &r.states, 24);
    spans.push(span("  Reports", t.dim()));
    spans.push(span(
        format!("  {} from subagents", r.states.len()),
        t.muted(),
    ));
    let failed = r.states.iter().filter(|s| **s == Sq::Failed).count();
    let right = if failed > 0 {
        vec![span(format!("{failed} did not finish"), t.error())]
    } else {
        vec![span("ctrl+o", t.faint())]
    };
    row(spans, right, env.width)
}

/// Past tense and nouns of a tool, for group summaries.
fn verb_noun(name: &str) -> (&'static str, &'static str, &'static str) {
    let p = ToolPresentation::for_call(name, &Value::Null);
    let n = p.kind.to_ascii_lowercase();
    let starts = |ps: &[&str]| ps.iter().any(|p| n.starts_with(p));
    if starts(&["bash", "shell", "exec", "run", "terminal", "command", "job"]) {
        ("Ran", "command", "commands")
    } else if starts(&["edit", "write", "patch", "create", "replace"]) {
        ("Edited", "file", "files")
    } else if starts(&["read", "view", "cat", "open"]) {
        ("Read", "file", "files")
    } else if starts(&["grep", "search", "glob", "find", "ls", "list", "recall"]) {
        ("Searched", "time", "times")
    } else if n == "browser" {
        ("Took", "browser step", "browser steps")
    } else if n == "recall" {
        ("Looked back", "time", "times")
    } else if starts(&["web", "fetch", "browse", "http", "url"]) {
        ("Browsed", "page", "pages")
    } else if starts(&["task", "plan", "todo"]) {
        ("Updated", "plan", "plans")
    } else if starts(&["memory", "remember"]) {
        ("Updated", "memory", "memories")
    } else if starts(&["message", "ask_bot", "dm"]) {
        ("Messaged", "bot", "bots")
    } else if n == "agent" || n == "agents" || starts(&["subagent", "delegate"]) {
        ("Delegated", "task", "tasks")
    } else if starts(&["whiteboard", "board"]) {
        ("Showed", "whiteboard", "whiteboards")
    } else {
        ("Used", "tool", "tools")
    }
}

/// "Ran 3 commands, edited 2 files".
fn group_summary(names: &[String]) -> String {
    let mut counts: Vec<((&str, &str, &str), usize)> = Vec::new();
    for n in names {
        let vn = verb_noun(n);
        match counts.iter_mut().find(|(k, _)| k.0 == vn.0 && k.1 == vn.1) {
            Some((_, c)) => *c += 1,
            None => counts.push((vn, 1)),
        }
    }
    let s = counts
        .iter()
        .map(|((verb, one, many), c)| {
            format!(
                "{} {c} {}",
                verb.to_lowercase(),
                if *c == 1 { one } else { many }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

/// A finished run of steps: "▪▪▪▪ 4 steps · 14s  Ran 2 commands, …".
fn steps_row(g: &StepsRow, env: &Env<'_>) -> Line<'static> {
    let t = env.theme;
    let n = g.tools.len();
    let mut spans = motif::strip(t, &g.states, 16);
    spans.push(span(
        if n == 0 {
            "  Work state".into()
        } else {
            format!("  {n} step{}", if n == 1 { "" } else { "s" })
        },
        t.muted(),
    ));
    if let Some(ms) = g.ms.filter(|ms| *ms >= 1000) {
        spans.push(span(format!(" · {}", fmt_ms(ms)), t.dim()));
    }
    spans.push(span(format!("  {}", group_summary(&g.tools)), t.dim()));
    let right = if g.failed > 0 {
        vec![span(format!("{} failed", g.failed), t.error())]
    } else {
        Vec::new()
    };
    row(spans, right, env.width)
}

fn compaction(summary: &str, before: u64, after: u64, env: &Env<'_>) -> Vec<Line<'static>> {
    let t = env.theme;
    let label = format!(
        " context compacted · {} → {} tokens ",
        text::tokens(before),
        text::tokens(after)
    );
    let lw = text::width(&label);
    let side = env.width.saturating_sub(lw) / 2;
    let side = side.clamp(2, 30);
    let mut out = vec![Line::from(vec![
        span("─".repeat(side), t.hairline()),
        span(label, t.faint()),
        span("─".repeat(side), t.hairline()),
    ])];
    if env.expand {
        let md = markdown::render(summary, env.width.saturating_sub(4), t);
        out.extend(indent(md, 4).into_iter().map(|l| l.patch_style(t.dim())));
    }
    out
}

/// Lines under a step row: a hairline gutter.
fn gutter(t: &Theme) -> Vec<Span<'static>> {
    vec![span("  │ ", t.hairline())]
}

/// Up to `limit` lines of output under a step, with a "+N lines" footer.
fn output_lines(
    text_s: &str,
    limit: usize,
    tail: bool,
    style: Style,
    env: &Env<'_>,
) -> Vec<Line<'static>> {
    let t = env.theme;
    let clean = text::sanitize(text_s);
    let all: Vec<&str> = clean.trim_matches('\n').lines().collect();
    if all.is_empty() || (all.len() == 1 && all[0].trim().is_empty()) || limit == 0 {
        return Vec::new();
    }
    let total = all.len();
    let shown: Vec<&str> = if total <= limit {
        all
    } else if tail {
        all[total - limit..].to_vec()
    } else {
        all[..limit].to_vec()
    };
    let mut out = Vec::new();
    if tail && total > limit {
        let mut spans = gutter(t);
        spans.push(span(
            format!("… {} earlier lines", total - limit),
            t.faint(),
        ));
        out.push(Line::from(spans));
    }
    let avail = env.width.saturating_sub(4);
    for l in shown {
        if env.expand {
            out.extend(text::hard_wrap(l, style, env.width, &gutter(t), &gutter(t)));
        } else {
            let mut spans = gutter(t);
            spans.push(span(text::truncate(l, avail), style));
            out.push(Line::from(spans));
        }
    }
    if !tail && total > limit {
        let mut spans = gutter(t);
        spans.push(span(
            format!(
                "… +{} lines{}",
                total - limit,
                if env.expand { "" } else { " · ctrl+o" }
            ),
            t.faint(),
        ));
        out.push(Line::from(spans));
    }
    out
}

/// Coloured mini diff from a unified diff. Returns (added, removed, lines).
fn diff_lines(diff: &str, limit: usize, env: &Env<'_>) -> (usize, usize, Vec<Line<'static>>) {
    let t = env.theme;
    let mut added = 0;
    let mut removed = 0;
    let mut rows: Vec<(Option<usize>, char, String)> = Vec::new();
    let (mut old_n, mut new_n) = (0usize, 0usize);
    let mut hunks = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") || l.starts_with("\\ ") {
            continue;
        }
        if let Some(h) = l.strip_prefix("@@") {
            // @@ -a,b +c,d @@
            let parts: Vec<&str> = h.split_whitespace().collect();
            let parse = |p: Option<&&str>| {
                p.and_then(|s| s.get(1..).and_then(|s| s.split(',').next()))
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(1)
            };
            old_n = parse(parts.first());
            new_n = parse(parts.get(1));
            if hunks > 0 {
                rows.push((None, '⋯', String::new()));
            }
            hunks += 1;
            continue;
        }
        let (sign, body) = match l.chars().next() {
            Some('+') => ('+', &l[1..]),
            Some('-') => ('-', &l[1..]),
            Some(' ') => (' ', &l[1..]),
            _ => (' ', l),
        };
        match sign {
            '+' => {
                added += 1;
                rows.push((Some(new_n), '+', body.to_string()));
                new_n += 1;
            }
            '-' => {
                removed += 1;
                rows.push((Some(old_n), '-', body.to_string()));
                old_n += 1;
            }
            _ => {
                rows.push((Some(new_n), ' ', body.to_string()));
                old_n += 1;
                new_n += 1;
            }
        }
    }
    let num_w = rows
        .iter()
        .filter_map(|r| r.0)
        .max()
        .unwrap_or(1)
        .to_string()
        .len()
        .max(2);
    let mut out = Vec::new();
    let total = rows.len();
    for (num, sign, body) in rows.iter().take(limit) {
        let mut spans = gutter(t);
        if *sign == '⋯' {
            spans.push(span(format!("{:>num_w$} ⋯", ""), t.faint()));
            out.push(Line::from(spans));
            continue;
        }
        let style = match sign {
            '+' => t.diff_add(),
            '-' => t.diff_del(),
            _ => t.dim(),
        };
        spans.push(span(format!("{:>num_w$} ", num.unwrap_or(0)), t.faint()));
        spans.push(span(format!("{sign} "), style));
        let clean = text::sanitize(body);
        let avail = env.width.saturating_sub(4 + num_w + 3);
        spans.push(span(text::truncate(&clean, avail), style));
        out.push(Line::from(spans));
    }
    if total > limit {
        let mut spans = gutter(t);
        spans.push(span(
            format!(
                "… +{} lines{}",
                total - limit,
                if env.expand { "" } else { " · ctrl+o" }
            ),
            t.faint(),
        ));
        out.push(Line::from(spans));
    }
    (added, removed, out)
}

/// A subagent an `agent` call started.
struct Child {
    name: String,
    id: Option<String>,
}

/// Where a subagent is: running, waiting for a slot, done or failed.
fn child_state(c: &Child, env: &Env<'_>, reports: &HashMap<String, bool>) -> Sq {
    let info =
        c.id.as_ref()
            .and_then(|id| env.children.iter().find(|s| &s.id == id));
    let report = c.id.as_ref().and_then(|id| reports.get(id)).copied();
    match info {
        Some(i) if super::pane::is_busy(i.status) => Sq::Running,
        Some(i) if matches!(i.status, SessionStatus::Error | SessionStatus::Interrupted) => {
            Sq::Failed
        }
        Some(i)
            if i.steps == 0
                && report.is_none()
                && i.helper_owner != Some(HelperOwner::ClaudeCode) =>
        {
            Sq::Waiting
        }
        Some(_) => {
            if report == Some(true) {
                Sq::Failed
            } else {
                Sq::Done
            }
        }
        None => match report {
            Some(true) => Sq::Failed,
            Some(false) => Sq::Done,
            None => Sq::Unknown,
        },
    }
}

/// The subagents of an `agent` call: one line each, or (a swarm of more
/// than six) a strip with counts.
fn subagent_lines(
    args: &Value,
    details: Option<&Value>,
    ts: u64,
    env: &Env<'_>,
    reports: &HashMap<String, bool>,
) -> Vec<Line<'static>> {
    let t = env.theme;
    let names: Vec<String> = args
        .get("tasks")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .enumerate()
                .map(|(i, x)| {
                    x.get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("task {}", i + 1))
                })
                .collect()
        })
        .unwrap_or_default();
    let kids: Vec<Child> = match details
        .and_then(|d| d.get("subagents"))
        .and_then(Value::as_array)
    {
        Some(list) => list
            .iter()
            .map(|x| Child {
                name: x
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Subagent")
                    .to_string(),
                id: x
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect(),
        None => env
            .children
            .iter()
            .filter(|c| c.created_at + 2000 >= ts)
            .take(names.len().max(1) * 2)
            .enumerate()
            .map(|(i, c)| Child {
                name: if c.title.trim().is_empty() {
                    names.get(i).cloned().unwrap_or_else(|| "Subagent".into())
                } else {
                    c.title.trim_start_matches("↳ ").to_string()
                },
                id: Some(c.id.clone()),
            })
            .collect(),
    };
    if kids.is_empty() {
        let meta = details.and_then(|d| d.get("subagent"));
        return names
            .iter()
            .map(|name| {
                let sq = if meta
                    .is_some_and(|m| m.get("background").and_then(Value::as_bool) == Some(true))
                {
                    Sq::Unknown
                } else if details.is_some() {
                    Sq::Done
                } else {
                    Sq::Waiting
                };
                let mut spans = vec![
                    span("  ", Style::new()),
                    motif::square(t, sq),
                    span(format!(" ↳ {name}"), t.muted()),
                ];
                if let Some(n) = meta
                    .and_then(|m| m.get("tool_uses"))
                    .and_then(Value::as_u64)
                {
                    spans.push(span(format!("  {n} steps"), t.faint()));
                }
                if let Some(ms) = meta
                    .and_then(|m| m.get("duration_ms"))
                    .and_then(Value::as_u64)
                    .filter(|ms| *ms >= 1000)
                {
                    spans.push(span(format!(" · {}", fmt_ms(ms)), t.faint()));
                }
                text::truncate_line(Line::from(spans), env.width)
            })
            .collect();
    }
    let states: Vec<Sq> = kids.iter().map(|c| child_state(c, env, reports)).collect();
    let mut out = Vec::new();
    let swarm = kids.len() > 6;
    if swarm {
        let count = |s: Sq| states.iter().filter(|x| **x == s).count();
        let mut parts = vec![format!("{} subagents", kids.len())];
        for (s, label) in [
            (Sq::Running, "running"),
            (Sq::Waiting, "waiting for a slot"),
            (Sq::Done, "done"),
            (Sq::Failed, "failed"),
        ] {
            let n = count(s);
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        let mut spans = vec![span("  ", Style::new())];
        spans.extend(motif::strip(t, &states, 24));
        spans.push(span(format!("  {}", parts.join(" · ")), t.dim()));
        out.push(text::truncate_line(Line::from(spans), env.width));
        if !env.expand {
            return out;
        }
    }
    for (c, s) in kids.iter().zip(&states) {
        let mut spans = vec![
            span("  ", Style::new()),
            motif::square(t, *s),
            span(" ", Style::new()),
        ];
        spans.push(span(text::sanitize(&c.name).into_owned(), t.muted()));
        let preview =
            c.id.as_ref()
                .and_then(|id| env.children.iter().find(|x| &x.id == id))
                .filter(|_| *s == Sq::Running)
                .map(|i| i.preview.clone())
                .unwrap_or_default();
        if !preview.is_empty() {
            spans.push(span(
                format!("  {}", text::sanitize(&preview).replace('\n', " ")),
                t.faint(),
            ));
        }
        out.push(text::truncate_line(Line::from(spans), env.width));
    }
    out
}

/// A tool step: one compact row (status tessera, name, what it did,
/// numbers on the right), with output only where it matters: live output
/// while it runs, the first lines of a failure, a short diff of an edit.
/// ctrl+o shows everything.
fn tool_card(
    name: &str,
    args: &Value,
    state: CallState<'_>,
    ts: u64,
    dur: Option<u64>,
    reports: &HashMap<String, bool>,
    env: &Env<'_>,
) -> Vec<Line<'static>> {
    let raw_name = name;
    let raw_args = args;
    let p = ToolPresentation::for_call(name, args);
    let name = p.kind.as_str();
    let args = &p.arguments;
    let t = env.theme;
    let summary = tool_summary(name, args, env.cwd);
    let sq = state.square();
    let mut meta: Vec<Span<'static>> = Vec::new();
    let mut body: Vec<Line<'static>> = Vec::new();
    let took = dur
        .filter(|d| *d >= 1000)
        .map(|d| span(fmt_ms(d), t.faint()));
    match state {
        CallState::Streaming => meta.push(span("preparing", t.faint())),
        CallState::Pending => {}
        CallState::NotRun => meta.push(span(
            if env.interrupted {
                "interrupted — outcome unknown"
            } else {
                "not run"
            },
            t.dim(),
        )),
        CallState::Approval => meta.push(span("needs approval", t.warning())),
        CallState::Running { output, elapsed } => {
            if elapsed.as_secs() >= 1 {
                meta.push(span(fmt_elapsed(elapsed), t.faint()));
            }
            let subs = if name == "agent" {
                subagent_lines(args, None, ts, env, reports)
            } else {
                Vec::new()
            };
            if subs.is_empty() {
                body.extend(output_lines(
                    output,
                    if env.expand { 40 } else { 4 },
                    true,
                    t.dim(),
                    env,
                ));
            } else {
                body.extend(subs);
            }
        }
        CallState::Done {
            content,
            is_error,
            details,
            ..
        } => {
            let interrupted = details
                .is_some_and(|d| d.get("interrupted").and_then(Value::as_bool) == Some(true));
            let stopped = details.is_some_and(|d| {
                d.get("stopped").and_then(Value::as_bool) == Some(true)
                    || d.get("status").and_then(Value::as_str) == Some("stopped")
                    || d.pointer("/subagent/status").and_then(Value::as_str) == Some("stopped")
            });
            if stopped {
                meta.push(span("Stopped", t.dim()));
            } else if interrupted {
                meta.push(span("Outcome unknown", t.warning()));
            }
            let text_s = content_text(content);
            let diff = details
                .and_then(|d| d.get("diff"))
                .and_then(Value::as_str)
                .filter(|d| !d.trim().is_empty());
            let full = if env.expand { 40 } else { 0 };
            match (name, is_error, diff) {
                // What the model is told after a "no" is for the model.
                (_, true, _) if text_s.starts_with(DECLINED) => {
                    meta.push(span("you declined", t.muted()));
                }
                ("edit" | "write", false, Some(d)) => {
                    let (a, r, lines) = diff_lines(d, if env.expand { 40 } else { 6 }, env);
                    meta.push(span(format!("+{a}"), t.diff_add()));
                    meta.push(span(format!(" −{r}"), t.diff_del()));
                    body.extend(lines);
                }
                ("read", false, _) => {
                    let first = text_s.lines().next().unwrap_or("");
                    let is_image = content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Image { .. }));
                    if is_image {
                        meta.push(span("image", t.faint()));
                    } else if let Some(n) = first
                        .strip_suffix(" entries)")
                        .and_then(|f| f.rsplit_once('('))
                        .map(|(_, n)| n.to_string())
                    {
                        // Directory listing: "<path> (N entries)" then one entry per line.
                        meta.push(span(format!("{n} entries"), t.faint()));
                        let rest = text_s.split_once('\n').map(|(_, r)| r).unwrap_or("");
                        body.extend(output_lines(rest, full, false, t.dim(), env));
                    } else {
                        let n = text_s.lines().count().saturating_sub(1);
                        meta.push(span(
                            format!("{n} line{}", if n == 1 { "" } else { "s" }),
                            t.faint(),
                        ));
                        body.extend(output_lines(&text_s, full, false, t.dim(), env));
                    }
                }
                ("bash" | "job", _, _) => {
                    let (out, status) = bash_status(&text_s);
                    let limit = if env.expand {
                        40
                    } else if is_error {
                        3
                    } else {
                        4
                    };
                    let exit = details
                        .and_then(|d| d.get("exit_code"))
                        .and_then(Value::as_i64)
                        .map(|n| format!("exit {n}"));
                    match status.or(exit.as_deref()) {
                        Some(s) => meta.push(span(
                            s.to_string(),
                            if is_error { t.error() } else { t.faint() },
                        )),
                        None => {
                            if let Some(d) = took.clone() {
                                meta.push(d);
                            }
                        }
                    }
                    body.extend(output_lines(
                        out,
                        limit,
                        false,
                        if is_error { t.error() } else { t.dim() },
                        env,
                    ));
                }
                ("glob", false, _) => {
                    let n = text_s.lines().filter(|l| !l.trim().is_empty()).count();
                    meta.push(span(
                        format!("{n} result{}", if n == 1 { "" } else { "s" }),
                        t.faint(),
                    ));
                    body.extend(output_lines(&text_s, full, false, t.dim(), env));
                }
                ("agent", false, _) => {
                    let subs = subagent_lines(args, details, ts, env, reports);
                    if subs.is_empty() {
                        body.extend(output_lines(&text_s, full, false, t.dim(), env));
                    } else {
                        body.extend(subs);
                    }
                }
                ("whiteboard", false, _) => {
                    if details.and_then(|d| d.get("board")).is_some() {
                        meta.push(span("shown beside", t.faint()));
                    }
                    body.extend(output_lines(&text_s, full, false, t.dim(), env));
                }
                ("browser", false, _) => {
                    let image = content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Image { .. }));
                    if image {
                        meta.push(span("screenshot", t.faint()));
                    } else {
                        let n = text_s.lines().filter(|l| !l.trim().is_empty()).count();
                        if n > 3 {
                            meta.push(span(format!("{n} lines"), t.faint()));
                        }
                    }
                    // Where it ended up, in one quiet line.
                    let lim = if env.expand { 40 } else { 1 };
                    body.extend(output_lines(&text_s, lim, false, t.dim(), env));
                }
                ("recall", false, _) => {
                    let hits = recall_hits(details, &text_s);
                    if text_s.trim() == "No matches." {
                        meta.push(span("no matches", t.faint()));
                    } else if !hits.is_empty() {
                        let n = hits.len();
                        meta.push(span(
                            format!("{n} match{}", if n == 1 { "" } else { "es" }),
                            t.faint(),
                        ));
                        let shown = if env.expand { hits.len() } else { 3 };
                        for h in hits.iter().take(shown) {
                            body.push(recall_hit(h, env));
                        }
                        if hits.len() > shown {
                            body.push(Line::from(span(
                                format!("    … {} more (ctrl+o)", hits.len() - shown),
                                t.faint(),
                            )));
                        }
                    } else {
                        body.extend(output_lines(&text_s, full, false, t.dim(), env));
                    }
                }
                (_, true, _) => {
                    meta.push(span("failed", t.error()));
                    body.extend(output_lines(
                        &text_s,
                        if env.expand { 40 } else { 2 },
                        false,
                        t.error(),
                        env,
                    ))
                }
                _ => body.extend(output_lines(&text_s, full, false, t.dim(), env)),
            }
            if !matches!(name, "bash" | "job")
                && let Some(d) = took
            {
                if !meta.is_empty() {
                    meta.push(span("  ", Style::new()));
                }
                meta.push(d);
            }
        }
    }
    let mut head = vec![
        motif::square(t, sq),
        span(" ", Style::new()),
        span(name.to_string(), t.muted()),
    ];
    if !summary.is_empty() {
        head.push(span("  ", Style::new()));
        head.push(span(text::sanitize(&summary).replace('\n', " "), t.dim()));
    }
    let mut out = vec![row(head, meta, env.width)];
    out.extend(body);
    if env.expand {
        out.push(Line::from(span(
            format!("    execution: {raw_name}"),
            t.faint(),
        )));
        for l in serde_json::to_string_pretty(raw_args)
            .unwrap_or_default()
            .lines()
        {
            out.extend(text::wrap_plain(
                l,
                t.faint(),
                env.width,
                &[span("    ", Style::new())],
                &[span("    ", Style::new())],
            ));
        }
    }
    out
}

/// One recall hit, as the card shows it.
#[derive(Debug, PartialEq)]
struct RecallHit {
    seq: String,
    /// The chat it is from (empty: this chat).
    title: String,
    /// When (empty in this chat).
    when: String,
    snippet: String,
}

/// The hits of a recall search: from the structured details when the
/// server sends them, else from the text (`seq 12 [Title · day · id]: …`).
fn recall_hits(details: Option<&Value>, text: &str) -> Vec<RecallHit> {
    if let Some(r) = details.and_then(|d| d.get("recall")) {
        let all = r.get("scope").and_then(Value::as_str) == Some("all");
        let s = |h: &Value, k: &str| h.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        return r
            .get("hits")
            .and_then(Value::as_array)
            .map(|hits| {
                hits.iter()
                    .map(|h| RecallHit {
                        seq: h.get("seq").map(|v| v.to_string()).unwrap_or_default(),
                        title: if all {
                            s(h, "session_title")
                        } else {
                            String::new()
                        },
                        when: match h.get("ts").and_then(Value::as_u64) {
                            Some(ts) if all => super::app::ago(ts),
                            _ => String::new(),
                        },
                        snippet: s(h, "snippet").replace('\n', " "),
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    text.lines()
        .filter_map(|l| l.strip_prefix("seq "))
        .map(|rest| {
            let (head, snippet) = rest.split_once(": ").unwrap_or((rest, ""));
            let (seq, place) = match head.split_once(" [") {
                Some((seq, p)) => (seq, p.trim_end_matches(']')),
                None => (head, ""),
            };
            let mut parts = place.split(" · ");
            RecallHit {
                seq: seq.to_string(),
                title: parts.next().unwrap_or("").to_string(),
                when: parts.next().unwrap_or("").to_string(),
                snippet: snippet.to_string(),
            }
        })
        .collect()
}

/// One recall hit as `Title · 2d ago — snippet` (or `#12 — snippet` in
/// this chat).
fn recall_hit(h: &RecallHit, env: &Env<'_>) -> Line<'static> {
    let t = env.theme;
    let mut spans = vec![span("    ", Style::new())];
    if h.title.is_empty() {
        spans.push(span(format!("#{}", h.seq), t.faint()));
    } else {
        spans.push(span(text::sanitize(&h.title).into_owned(), t.muted()));
        if !h.when.is_empty() {
            spans.push(span(format!(" · {}", h.when), t.faint()));
        }
    }
    spans.push(span(" — ", t.faint()));
    // The search marks matches «like this»: show them as highlights.
    // Markdown marks and runs of spaces are noise in one line.
    let snippet = text::sanitize(&h.snippet)
        .replace("**", "")
        .split_whitespace()
        .filter(|w| !w.chars().all(|c| c == '#'))
        .collect::<Vec<_>>()
        .join(" ");
    for (i, part) in snippet.split(['«', '»']).enumerate() {
        if part.is_empty() {
            continue;
        }
        spans.push(span(
            part.to_string(),
            if i % 2 == 1 { t.accent_text() } else { t.dim() },
        ));
    }
    text::truncate_line(Line::from(spans), env.width)
}

/// A tool call that is still being written by the model.
pub fn streaming_call(name: &str, raw_args: &Value, env: &Env<'_>) -> Vec<Line<'static>> {
    tool_card(
        name,
        raw_args,
        CallState::Streaming,
        0,
        None,
        &HashMap::new(),
        env,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::text::line_text;
    use serde_json::json;

    fn no_bots() -> &'static HashMap<String, BotLook> {
        static EMPTY: std::sync::OnceLock<HashMap<String, BotLook>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }

    fn no_ms() -> &'static HashMap<u64, u64> {
        static EMPTY: std::sync::OnceLock<HashMap<u64, u64>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }

    fn env<'a>(theme: &'a Theme, running: &'a HashMap<String, RunningTool>) -> Env<'a> {
        Env {
            theme,
            width: 60,
            expand: false,
            thinking: false,
            frame: 0,
            running,
            approvals: &[],
            active: false,
            now: Instant::now(),
            children: &[],
            cwd: "/proj",
            interrupted: false,
            bots: no_bots(),
            bot: None,
            channel: false,
            think_ms: no_ms(),
        }
    }

    fn entry(seq: u64, body: EntryBody) -> Entry {
        Entry { seq, ts: 0, body }
    }

    fn model() -> ModelRef {
        ModelRef {
            provider: "p".into(),
            model: "m".into(),
        }
    }

    fn asst(content: Vec<AssistantPart>, author: Option<&str>) -> EntryBody {
        EntryBody::Assistant {
            content,
            model: model(),
            usage: Usage::default(),
            stop: StopReason::Stop,
            error: None,
            author: author.map(str::to_string),
        }
    }

    fn call(id: &str, name: &str, args: Value) -> AssistantPart {
        AssistantPart::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }
    }

    fn result(call_id: &str, tool: &str, text: &str, is_error: bool) -> EntryBody {
        EntryBody::ToolResult {
            call_id: call_id.into(),
            tool: tool.into(),
            content: vec![ContentPart::text(text)],
            is_error,
            artifact: None,
            details: None,
        }
    }

    fn user(text: &str) -> EntryBody {
        EntryBody::User {
            content: vec![ContentPart::text(text)],
            author: None,
        }
    }

    fn lines(t: &Transcript) -> Vec<String> {
        t.blocks()
            .flatten()
            .map(|l| line_text(l).trim_end().to_string())
            .collect()
    }

    #[test]
    fn claude_style_ack_without_a_harness_question_is_never_a_row_or_last_answer() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let mut t = Transcript::default();
        let text = |s: &str| AssistantPart::Text { text: s.into() };
        t.add_entry(entry(1, asst(vec![text("Found the problem.")], None)));
        t.add_entry(entry(2, asst(vec![text("✓")], None)));
        let mut marked = entry(3, asst(vec![text("An internal response")], None));
        if let EntryBody::Assistant { author, .. } = &mut marked.body {
            *author = Some(tessera_core::tools::meta::CHECK_AUTHOR.into());
        }
        t.add_entry(marked);
        t.prepare(&e);
        let shown = lines(&t).join("\n");
        assert!(shown.contains("Found the problem."));
        assert!(
            !shown.contains('✓') && !shown.contains("internal response"),
            "{shown}"
        );
        assert_eq!(
            t.last_assistant_text().as_deref(),
            Some("Found the problem.")
        );
        t.add_entry(entry(4, asst(vec![text("✓ Tests pass")], None)));
        t.prepare(&e);
        assert!(lines(&t).join("\n").contains("Tests pass"));
    }

    #[test]
    fn paired_native_claude_tool_cards_all_states() {
        let f: Value = serde_json::from_str(include_str!(
            "../../../../apps/tessera/ui/tests/fixtures/chat-presentation.json"
        ))
        .unwrap();
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        for fixture in f["calls"].as_array().unwrap() {
            let content = vec![ContentPart::text(fixture["output"].as_str().unwrap())];
            let native_content = vec![ContentPart::text(
                fixture
                    .get("native_output")
                    .and_then(Value::as_str)
                    .unwrap_or(fixture["output"].as_str().unwrap()),
            )];
            for status in f["states"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
            {
                let mut details = fixture.get("details").cloned().unwrap_or_else(|| json!({}));
                if status == "stopped" {
                    details["status"] = json!("stopped");
                }
                let draw = |engine: &str| {
                    let c = &fixture[engine];
                    let state = match status {
                        "start" => CallState::Streaming,
                        "pending" => CallState::Pending,
                        "running" => CallState::Running {
                            output: "Inspecting src/app.rs\n",
                            elapsed: Duration::from_millis(2400),
                        },
                        "approval" => CallState::Approval,
                        "unknown" => CallState::NotRun,
                        _ => CallState::Done {
                            content: if engine == "native" {
                                &native_content
                            } else {
                                &content
                            },
                            is_error: status == "error",
                            details: Some(&details),
                            seq: 1,
                        },
                    };
                    tool_card(
                        c["name"].as_str().unwrap(),
                        &c["args"],
                        state,
                        0,
                        None,
                        &HashMap::new(),
                        &e,
                    )
                };
                assert_eq!(
                    draw("native"),
                    draw("claude"),
                    "{} {status}",
                    fixture["claude"]["name"]
                );
            }
        }
        let call = &f["calls"][0];
        let content = vec![ContentPart::text("hello\n")];
        let drawn = tool_card(
            "Bash",
            &call["claude"]["args"],
            CallState::Done {
                content: &content,
                is_error: false,
                details: call.get("details"),
                seq: 1,
            },
            0,
            None,
            &HashMap::new(),
            &e,
        );
        let rendered = drawn.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            rendered.contains("hello") && rendered.contains("exit 0"),
            "{rendered}"
        );
        let mut expanded = e;
        expanded.expand = true;
        let drawn = tool_card(
            "Edit",
            &f["calls"][4]["claude"]["args"],
            CallState::Done {
                content: &content,
                is_error: false,
                details: f["calls"][4].get("details"),
                seq: 1,
            },
            0,
            None,
            &HashMap::new(),
            &expanded,
        );
        let rendered = drawn.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            rendered.contains("- old")
                && rendered.contains("+ new")
                && rendered.contains("execution: Edit")
                && rendered.contains("file_path"),
            "{rendered}"
        );
    }

    #[test]
    fn housekeeping_folds_without_inflating_steps_and_keeps_audit() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        for ledger in ["task", "TaskCreate", "ToolSearch"] {
            let mut t = Transcript::default();
            t.add_entry(entry(
                1,
                asst(vec![call("a", ledger, json!({"subject":"Review"}))], None),
            ));
            t.add_entry(entry(2, result("a", ledger, "recorded", false)));
            t.prepare(&e);
            let text = lines(&t).join("\n");
            assert!(
                text.contains("Work state") && !text.contains("1 step"),
                "{text}"
            );
            let expanded = Env { expand: true, ..e };
            t.prepare(&expanded);
            assert!(
                lines(&t)
                    .join("\n")
                    .contains(&format!("execution: {ledger}"))
            );
        }
    }

    #[test]
    fn timestamps_are_not_execution_durations() {
        let mut t = Transcript::default();
        t.add_entry(Entry {
            seq: 1,
            ts: 1000,
            body: asst(vec![call("a", "bash", json!({"command":"echo hi"}))], None),
        });
        t.add_entry(Entry {
            seq: 2,
            ts: 100000,
            body: result("a", "bash", "hi", false),
        });
        t.derive();
        assert!(t.durations.is_empty());
        let mut r = result("a", "bash", "hi", false);
        if let EntryBody::ToolResult { details, .. } = &mut r {
            *details = Some(json!({"duration_ms":2400}));
        }
        t.replace_entry(Entry {
            seq: 2,
            ts: 100000,
            body: r,
        });
        t.derive();
        assert_eq!(t.durations["a"], 2400);
    }

    #[test]
    fn completion_checks_answered_with_a_mark_fold_away() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let text = |s: &str| AssistantPart::Text { text: s.into() };
        let check = |seq| {
            entry(
                seq,
                EntryBody::User {
                    content: vec![ContentPart::text(
                        "[Completion check] Still open in the task ledger:\nPlan:\n2. Run the tests [pending]\n3. Ship it [pending]\nIf these are done, record it with `task`.",
                    )],
                    author: Some("harness".into()),
                },
            )
        };
        let mut t = Transcript::default();
        t.add_entry(entry(1, user("fix the parser")));
        t.add_entry(entry(
            2,
            asst(vec![text("Fixed: years have four digits now.")], None),
        ));
        // A quiet answer: a ledger update and the mark.
        t.add_entry(check(3));
        assert!(t.quiet_check_open(), "the answer is on its way");
        t.add_entry(entry(
            4,
            asst(vec![call("c1", "task", json!({"update": []}))], None),
        ));
        t.add_entry(entry(5, result("c1", "task", "ok", false)));
        t.add_entry(entry(6, asst(vec![text("✓")], None)));
        t.prepare(&e);
        let shown = lines(&t).join("\n");
        assert!(
            shown.contains("Fixed: years have four digits now."),
            "{shown}"
        );
        for gone in ["Completion check", "✓", "task"] {
            assert!(!shown.contains(gone), "{gone}: {shown}");
        }
        assert_eq!(
            t.last_assistant_text().as_deref(),
            Some("Fixed: years have four digits now.")
        );
        // A check the agent answered with more work stays, in plain words.
        t.add_entry(entry(7, user("and the docs")));
        t.add_entry(entry(8, asst(vec![text("Done with the docs.")], None)));
        t.add_entry(check(9));
        t.add_entry(entry(
            10,
            asst(vec![text("The tests still fail; looking.")], None),
        ));
        assert!(!t.quiet_check_open());
        t.prepare(&e);
        let shown = lines(&t);
        assert!(
            !shown.iter().any(|l| l.contains("Completion check")),
            "{shown:?}"
        );
        assert!(!shown.iter().any(|l| l.contains("[Completion check]")));
        assert_eq!(
            t.last_assistant_text().as_deref(),
            Some("The tests still fail; looking.")
        );
        // Other notes from the harness: their label and first words.
        let mut t = Transcript::default();
        t.add_entry(entry(
            1,
            EntryBody::User {
                content: vec![ContentPart::text(
                    "[Budget reached] You have used the step budget.",
                )],
                author: Some("harness".into()),
            },
        ));
        t.prepare(&e);
        assert_eq!(
            lines(&t).iter().find(|l| !l.is_empty()).map(String::as_str),
            Some("  ▪ Budget reached · You have used the step budget.")
        );
    }

    #[test]
    fn summaries() {
        let c = "/proj";
        assert_eq!(
            tool_summary("bash", &json!({"command": "ls -la\necho hi"}), c),
            "ls -la …"
        );
        assert_eq!(
            tool_summary("bash", &json!({"command": "cd /proj && make"}), c),
            "make"
        );
        assert_eq!(
            tool_summary("bash", &json!({"command": "cd /other && make"}), c),
            "cd /other && make"
        );
        assert_eq!(
            tool_summary(
                "read",
                &json!({"path": "src/main.rs", "offset": 10, "limit": 5}),
                c
            ),
            "src/main.rs:10-14"
        );
        assert_eq!(
            tool_summary("read", &json!({"path": "/proj/src/a.rs"}), c),
            "src/a.rs"
        );
        assert_eq!(tool_summary("read", &json!({"path": "/proj"}), c), ".");
        assert_eq!(
            tool_summary("read", &json!({"paths": ["/proj/a.rs", "b.md"]}), c),
            "a.rs, b.md"
        );
        assert_eq!(
            tool_summary(
                "read",
                &json!({"path": "a.rs", "paths": ["b.md", "a.rs"]}),
                c
            ),
            "a.rs, b.md"
        );
        assert_eq!(
            tool_summary("read", &json!({"path": "/project/x"}), c),
            "/project/x"
        );
        assert_eq!(
            tool_summary("grep", &json!({"pattern": "fn main", "path": "src"}), c),
            "fn main  in src"
        );
        assert_eq!(
            tool_summary(
                "agent",
                &json!({"tasks": [{"name": "a", "prompt": "x"}, {"prompt": "y"}]}),
                c
            ),
            "2 subagents: a"
        );
        assert_eq!(
            tool_summary(
                "whiteboard",
                &json!({"action": "show", "title": "Pipeline"}),
                c
            ),
            "show · Pipeline"
        );
        // Streaming (incomplete) arguments.
        assert_eq!(
            tool_summary("bash", &Value::String(r#"{"command": "cargo te"#.into()), c),
            "cargo te"
        );
        assert_eq!(
            tool_summary(
                "edit",
                &Value::String(r#"{"path":"/proj/a\"b.rs","#.into()),
                c
            ),
            "a\"b.rs"
        );
        assert_eq!(tool_summary("bash", &Value::String("{".into()), c), "");
    }

    #[test]
    fn tool_rows_attach_results() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let mut t = Transcript::default();
        t.add_entry(entry(1, user("hi")));
        t.add_entry(entry(
            2,
            asst(
                vec![call("c1", "bash", json!({"command": "echo hi"}))],
                None,
            ),
        ));
        t.prepare(&e);
        let text = lines(&t);
        assert!(
            text.iter()
                .any(|l| l.starts_with("□ bash  echo hi") && l.ends_with("not run")),
            "{text:?}"
        );
        t.add_entry(entry(3, result("c1", "bash", "hi\n[exit 0 · 0.1s]", false)));
        t.prepare(&e);
        let text = lines(&t);
        let row = text
            .iter()
            .find(|l| l.starts_with("■ bash  echo hi"))
            .unwrap();
        // Old text-only results have no authoritative timing metadata.
        assert!(row.ends_with("exit 0"), "{text:?}");
        assert_eq!(row.chars().count(), 60, "numbers sit at the right edge");
        // Compact: both engines show the same short successful shell output.
        assert!(text.iter().any(|l| l.contains("│ hi")), "{text:?}");
        // …and ctrl+o shows the full output and execution identity.
        let ex = Env {
            expand: true,
            ..env(&theme, &running)
        };
        t.prepare(&ex);
        assert!(lines(&t).iter().any(|l| l == "  │ hi"), "{:?}", lines(&t));
        // The result entry itself renders nothing; duplicates are ignored.
        assert!(!t.add_entry(entry(
            3,
            EntryBody::Notice {
                level: NoticeLevel::Info,
                text: "x".into(),
                learned: Vec::new(),
            }
        )));
    }

    #[test]
    fn edit_diff_preview() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let diff =
            "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n fn a() {\n-    old();\n+    new();\n }\n";
        let lines = tool_card(
            "edit",
            &json!({"path": "x.rs"}),
            CallState::Done {
                content: &[],
                is_error: false,
                details: Some(&json!({"diff": diff})),
                seq: 1,
            },
            0,
            Some(3200),
            &HashMap::new(),
            &e,
        );
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].starts_with("■ edit  x.rs"), "{text:?}");
        assert!(text[0].ends_with("+1 −1  3s"), "{text:?}");
        assert_eq!(text[1], "  │  1   fn a() {");
        assert_eq!(text[2], "  │  2 -     old();");
        assert_eq!(text[3], "  │  2 +     new();");
    }

    #[test]
    fn failures_show_their_first_lines() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let out: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let content = vec![ContentPart::text(out)];
        let state = || CallState::Done {
            content: &content,
            is_error: true,
            details: None,
            seq: 1,
        };
        let lines = tool_card(
            "grep",
            &json!({"pattern": "x"}),
            state(),
            0,
            None,
            &HashMap::new(),
            &e,
        );
        assert_eq!(lines.len(), 1 + 2 + 1);
        assert!(line_text(&lines[0]).ends_with("failed"));
        assert!(line_text(&lines[3]).contains("+48 lines · ctrl+o"));
        let ex = Env {
            expand: true,
            ..env(&theme, &running)
        };
        let lines = tool_card(
            "grep",
            &json!({"pattern": "x"}),
            state(),
            0,
            None,
            &HashMap::new(),
            &ex,
        );
        assert!(lines.len() > 1 + 40);
        assert!(
            lines
                .iter()
                .map(line_text)
                .collect::<Vec<_>>()
                .join("\n")
                .contains("execution: grep")
        );
        // A "no" to an approval: said in a word, not in the model's
        // instructions.
        let content = vec![ContentPart::text(
            "The user declined this action. Do not retry it; continue another way or ask the user how to proceed.",
        )];
        let lines = tool_card(
            "bash",
            &json!({"command": "rm -rf build/"}),
            CallState::Done {
                content: &content,
                is_error: true,
                details: None,
                seq: 1,
            },
            0,
            None,
            &HashMap::new(),
            &e,
        );
        assert_eq!(
            lines.len(),
            1,
            "{:?}",
            lines.iter().map(line_text).collect::<Vec<_>>()
        );
        assert!(line_text(&lines[0]).ends_with("you declined"));
    }

    #[test]
    fn finished_steps_fold_into_one_row() {
        let theme = Theme::test();
        let running = HashMap::new();
        let mut t = Transcript::default();
        t.add_entry(entry(1, user("fix it")));
        t.add_entry(entry(
            2,
            asst(
                vec![
                    AssistantPart::Thinking {
                        text: "look first".into(),
                        opaque: None,
                    },
                    call("a", "bash", json!({"command": "ls"})),
                    call("b", "read", json!({"path": "/proj/x.rs"})),
                ],
                None,
            ),
        ));
        t.add_entry(entry(
            3,
            result("a", "bash", "x.rs\n[exit 0 · 0.1s]", false),
        ));
        t.add_entry(entry(4, result("b", "read", "1\n2", false)));
        t.add_entry(entry(
            5,
            asst(vec![call("c", "edit", json!({"path": "/proj/x.rs"}))], None),
        ));
        t.add_entry(entry(6, result("c", "edit", "no match", true)));
        // Still working: the latest steps stay open.
        let busy = Env {
            active: true,
            ..env(&theme, &running)
        };
        t.prepare(&busy);
        let text = lines(&t);
        assert!(text.iter().any(|l| l.starts_with("■ bash  ls")), "{text:?}");
        assert!(
            text.iter().any(|l| l.starts_with("■ edit  x.rs")),
            "{text:?}"
        );
        t.add_entry(entry(
            7,
            asst(
                vec![
                    AssistantPart::Thinking {
                        text: "done".into(),
                        opaque: None,
                    },
                    AssistantPart::Text {
                        text: "All fixed.".into(),
                    },
                ],
                None,
            ),
        ));
        // Finished: one row, then the answer.
        let e = Env {
            width: 80,
            ..env(&theme, &running)
        };
        t.prepare(&e);
        let text = lines(&t);
        assert_eq!(text.len(), 6, "{text:?}");
        assert_eq!(text[1], "▎ fix it");
        assert!(
            text[3].starts_with("▪▪▪▪▪  3 steps  Ran 1 command, read 1 file, edited 1 file"),
            "{text:?}"
        );
        assert!(text[3].ends_with("1 failed"), "{text:?}");
        assert_eq!(text[5], "  All fixed.");
        // ctrl+o unfolds.
        let ex = Env {
            expand: true,
            ..env(&theme, &running)
        };
        t.prepare(&ex);
        let text = lines(&t);
        assert!(
            text.iter().any(|l| l.starts_with("■ read  x.rs")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with("◇ Thought process")),
            "{text:?}"
        );
        // A single step doesn't fold.
        let mut one = Transcript::default();
        one.add_entry(entry(
            1,
            asst(vec![call("a", "bash", json!({"command": "ls"}))], None),
        ));
        one.add_entry(entry(2, result("a", "bash", "[exit 0 · 0.1s]", false)));
        one.prepare(&e);
        assert!(lines(&one).iter().any(|l| l.starts_with("■ bash  ls")));
    }

    #[test]
    fn reports_goals_and_learned_notices() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let report = |seq, child: &str, body: &str| {
            entry(
                seq,
                EntryBody::User {
                    content: vec![ContentPart::text(format!(
                        "[Report from subagent \"Audit {child}\" · 2 steps · 4 s · session {child}]\n\n{body}"
                    ))],
                    author: Some(format!("subagent:{child}")),
                },
            )
        };
        let mut t = Transcript::default();
        t.add_entry(report(1, "c1", "**Verdict:** clean"));
        t.prepare(&e);
        let text = lines(&t);
        assert!(text[1].starts_with("↳ Report from Audit c1"), "{text:?}");
        assert!(text[1].ends_with("2 steps · 4 s"), "{text:?}");
        assert_eq!(text[2], "  │ Verdict: clean");
        // Three in a row fold.
        t.add_entry(report(2, "c2", "clean"));
        t.add_entry(report(3, "c3", "The subagent did not finish: timeout"));
        t.prepare(&e);
        let text = lines(&t);
        assert_eq!(text.iter().filter(|l| !l.is_empty()).count(), 1, "{text:?}");
        assert!(
            text[1].starts_with("▪▪▪  Reports  3 from subagents"),
            "{text:?}"
        );
        assert!(text[1].ends_with("1 did not finish"), "{text:?}");
        // Goal notes and memory reviews.
        let mut g = Transcript::default();
        g.add_entry(entry(
            1,
            EntryBody::User {
                content: vec![ContentPart::text(
                    "[The user set the goal for this session]\nShip the TUI",
                )],
                author: Some("goal".into()),
            },
        ));
        g.add_entry(entry(
            2,
            EntryBody::Notice {
                level: NoticeLevel::Info,
                text: "Learned from this conversation — about you: likes tea; skill: deploy".into(),
                learned: vec![
                    LearnedItem {
                        kind: LearnedKind::Profile,
                        action: LearnedAction::Added,
                        text: "Likes tea".into(),
                    },
                    LearnedItem {
                        kind: LearnedKind::Skill,
                        action: LearnedAction::Added,
                        text: "deploy".into(),
                    },
                ],
            },
        ));
        g.add_entry(entry(
            3,
            EntryBody::Notice {
                level: NoticeLevel::Info,
                text: "Learned from this conversation — note: uses tmux".into(),
                learned: Vec::new(),
            },
        ));
        g.prepare(&e);
        let text = lines(&g);
        assert!(
            text.contains(&"  ◆ Goal · Ship the TUI".to_string()),
            "{text:?}"
        );
        assert!(
            text.contains(&"  ◆ Remembered 2 things  ctrl+o".to_string()),
            "{text:?}"
        );
        assert!(
            text.contains(&"  ◆ Remembered 1 thing  ctrl+o".to_string()),
            "{text:?}"
        );
        let ex = Env {
            expand: true,
            ..env(&theme, &running)
        };
        g.prepare(&ex);
        let text = lines(&g);
        assert!(
            text.contains(&"    About you Likes tea".to_string()),
            "{text:?}"
        );
        assert!(
            text.contains(&"    Skill     deploy".to_string()),
            "{text:?}"
        );
        assert!(
            text.contains(&"    Note      uses tmux".to_string()),
            "{text:?}"
        );
    }

    #[test]
    fn thinking_is_one_quiet_line() {
        let theme = Theme::test();
        let running = HashMap::new();
        let mut ms = HashMap::new();
        ms.insert(1u64, 4200u64);
        let e = Env {
            think_ms: &ms,
            ..env(&theme, &running)
        };
        let mut t = Transcript::default();
        t.add_entry(entry(
            1,
            asst(
                vec![
                    AssistantPart::Thinking {
                        text: "hmm\nlet me think".into(),
                        opaque: None,
                    },
                    AssistantPart::Text {
                        text: "Answer".into(),
                    },
                ],
                None,
            ),
        ));
        t.prepare(&e);
        assert_eq!(lines(&t), vec!["", "◇ Thought for 4s", "", "  Answer"]);
        let show = Env {
            thinking: true,
            think_ms: &ms,
            ..env(&theme, &running)
        };
        t.prepare(&show);
        assert_eq!(
            lines(&t),
            vec![
                "",
                "◇ Thought for 4s",
                "│ hmm",
                "│ let me think",
                "",
                "  Answer"
            ]
        );
        // Live: the indicator, the time and the last line.
        let live = live_thinking(
            "first\nsecond **bold**",
            Some(e.now - Duration::from_secs(3)),
            None,
            &e,
        );
        assert_eq!(line_text(&live[0]), "▪▪▪ Thinking · 3s  second bold");
    }

    #[test]
    fn caches_thousands_of_entries() {
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let mut t = Transcript::default();
        let md = "Some **markdown** with `code` and a [link](https://x.dev).\n\n- one\n- two\n\n```rust\nfn main() {}\n```";
        for i in 0..2500u64 {
            t.add_entry(entry(i * 2 + 1, user(&format!("question {i}"))));
            t.add_entry(entry(
                i * 2 + 2,
                asst(vec![AssistantPart::Text { text: md.into() }], None),
            ));
        }
        let start = Instant::now();
        let total = t.prepare(&e);
        let first = start.elapsed();
        assert_eq!(t.renders, 5000);
        let start = Instant::now();
        assert_eq!(t.prepare(&e), total);
        let second = start.elapsed();
        assert_eq!(t.renders, 5000, "unchanged items must come from the cache");
        eprintln!(
            "5000 entries: first prepare {first:?}, cached prepare {second:?}, {total} lines"
        );
        // A width change re-renders everything once.
        let e2 = Env { width: 40, ..e };
        t.prepare(&e2);
        assert_eq!(t.renders, 10_000);
    }

    #[test]
    fn browser_and_recall_cards_read_well() {
        let j = |v: serde_json::Value| v;
        let sum = |n: &str, v| tool_summary(n, &j(v), "/home/u");
        assert_eq!(
            sum(
                "browser",
                serde_json::json!({"action": "open", "url": "https://www.example.com/a/?x=1", "show": true})
            ),
            "open · example.com/a  (beside)"
        );
        assert_eq!(
            sum(
                "browser",
                serde_json::json!({"action": "click", "ref": "12"})
            ),
            "click · @12"
        );
        assert_eq!(
            sum(
                "browser",
                serde_json::json!({"action": "type", "ref": "4", "text": "hello", "submit": true})
            ),
            "type · “hello” into @4 ⏎"
        );
        assert_eq!(
            sum(
                "browser",
                serde_json::json!({"action": "press", "key": "Enter"})
            ),
            "press · Enter"
        );
        assert_eq!(
            sum("browser", serde_json::json!({"action": "screenshot"})),
            "screenshot"
        );
        assert_eq!(
            sum(
                "recall",
                serde_json::json!({"query": "rust cache", "scope": "all", "since": "7d"})
            ),
            "“rust cache”  in all chats  since 7d"
        );
        assert_eq!(verb_noun("browser").0, "Took");
        // Recall hits become one tidy line each.
        let theme = Theme::test();
        let running = HashMap::new();
        let e = env(&theme, &running);
        let hits = recall_hits(
            None,
            "seq 12 [Why is CI slow? · 2026-09-30 · s1]: the build cache was off\nseq 3: earlier",
        );
        let s = text::line_text(&recall_hit(&hits[0], &e));
        assert!(
            s.contains("Why is CI slow? · 2026-09-30 — the build cache was off"),
            "{s}"
        );
        assert!(text::line_text(&recall_hit(&hits[1], &e)).contains("#3 — earlier"));
        let tidy = recall_hits(None, "seq 5:  ## Why  **341** crates");
        assert!(text::line_text(&recall_hit(&tidy[0], &e)).contains("#5 — Why 341 crates"));
        // The structured details win over the text.
        let d = serde_json::json!({"recall": {"query": "cache", "scope": "all", "hits": [
            {"session_id": "s1", "session_title": "Why is CI slow?", "seq": 12,
             "ts": tessera_core::util::now_ms() - 2 * 86_400_000, "snippet": "the «cache» was off"}
        ]}});
        let hits = recall_hits(Some(&d), "ignored");
        let s = text::line_text(&recall_hit(&hits[0], &e));
        assert!(
            s.contains("Why is CI slow? · 2d ago — the cache was off"),
            "{s}"
        );
        let d = serde_json::json!({"recall": {"query": "x", "scope": "session", "hits": [
            {"session_id": "s1", "session_title": "T", "seq": 4, "ts": 1, "snippet": "here"}
        ]}});
        let s = text::line_text(&recall_hit(&recall_hits(Some(&d), "")[0], &e));
        assert!(s.contains("#4 — here"), "{s}");
    }

    #[test]
    fn model_retries_share_one_line() {
        let mut t = Transcript::default();
        t.add_local(
            NoticeLevel::Warning,
            "HTTP 503: busy — retrying in 2s (attempt 1/5)",
        );
        t.add_local(
            NoticeLevel::Warning,
            "HTTP 503: busy — retrying in 4s (attempt 2/5)",
        );
        assert_eq!(t.items.len(), 1, "the second retry replaces the first");
        match &t.items[0] {
            Item::Local { text, .. } => assert!(text.contains("attempt 2/5")),
            _ => panic!("a local notice"),
        }
        t.add_local(NoticeLevel::Error, "something else");
        t.add_local(
            NoticeLevel::Warning,
            "HTTP 503: busy — retrying in 2s (attempt 1/5)",
        );
        assert_eq!(t.items.len(), 3, "other notices stay");
    }

    #[test]
    fn speakers_are_labelled_in_rooms_and_bot_chats() {
        let theme = Theme::test();
        let running = HashMap::new();
        let look = |name: &str, handle: &str, avatar: &str| {
            let mut b = BotLook::new(
                &Bot {
                    id: handle.into(),
                    name: name.into(),
                    handle: handle.into(),
                    avatar: avatar.into(),
                    color: "#ffffff".into(),
                    role: String::new(),
                    personality: String::new(),
                    instructions: String::new(),
                    model: None,
                    thinking: None,
                    permission: PermissionMode::Default,
                    tools: ToolPreset::Full,
                    allowed_tools: Vec::new(),
                    cwd: None,
                    own_memory: true,
                    share_user_profile: true,
                    can_message: Vec::new(),
                    askable: None,
                    knowledge: Vec::new(),
                    starters: Vec::new(),
                    machines: None,
                    secrets: None,
                    created_at: 0,
                    updated_at: 0,
                },
                &theme,
            );
            b.style = Style::new();
            b
        };
        let mut bots = HashMap::new();
        bots.insert("b1".to_string(), look("Nova", "nova", "🦊"));
        bots.insert("b2".to_string(), look("Forge", "forge", "m:laurel"));
        let text_msg =
            |t: &str, a: Option<&str>| asst(vec![AssistantPart::Text { text: t.into() }], a);
        let mut t = Transcript::default();
        t.add_entry(entry(1, user("@nova @forge hello")));
        t.add_entry(entry(2, text_msg("hi from nova", Some("b1"))));
        t.add_entry(entry(3, text_msg("hi from forge", Some("b2"))));
        t.add_entry(entry(4, text_msg("more from forge", Some("b2"))));
        t.add_entry(entry(
            5,
            EntryBody::User {
                content: vec![ContentPart::text("nova asks forge")],
                author: Some("b1".into()),
            },
        ));
        // A room: your messages are the gold-barred ones; each bot is named
        // once per turn.
        let room = Env {
            bots: &bots,
            channel: true,
            ..env(&theme, &running)
        };
        t.prepare(&room);
        let text = lines(&t);
        assert!(text.iter().any(|l| l == "▎ @nova @forge hello"), "{text:?}");
        assert_eq!(
            text.iter()
                .filter(|l| l.starts_with("● Nova  @nova"))
                .count(),
            2,
            "{text:?}"
        );
        assert_eq!(
            text.iter()
                .filter(|l| l.starts_with("● Forge  @forge"))
                .count(),
            1,
            "{text:?}"
        );
        assert!(text.iter().any(|l| l == "  nova asks forge"), "{text:?}");
        assert_eq!(t.last_speaker(), Some(&Speaker::Bot("b1".into())));
        // A bot chat: the bot is named once.
        let mut chat = Transcript::default();
        chat.add_entry(entry(1, user("hey")));
        chat.add_entry(entry(2, text_msg("one", None)));
        chat.add_entry(entry(3, text_msg("two", None)));
        let e = Env {
            bots: &bots,
            bot: Some("b1"),
            ..env(&theme, &running)
        };
        chat.prepare(&e);
        let text = lines(&chat);
        assert!(text.iter().any(|l| l == "▎ hey"), "{text:?}");
        assert_eq!(text.iter().filter(|l| l.starts_with("● Nova")).count(), 1);
        // Plain chats: no labels at all.
        let mut plain = Transcript::default();
        plain.add_entry(entry(1, user("hey")));
        plain.add_entry(entry(2, text_msg("answer", None)));
        plain.prepare(&env(&theme, &running));
        assert_eq!(lines(&plain), vec!["", "▎ hey", "", "  answer"]);
        // A bot DM: what a chat's agent asked is never "you", also from an
        // older server that left the author out.
        let mut dm = Transcript::default();
        dm.add_entry(entry(
            1,
            EntryBody::User {
                content: vec![ContentPart::text("check the backups")],
                author: Some("chat:ses_1".into()),
            },
        ));
        dm.add_entry(entry(2, text_msg("all fine", None)));
        dm.add_entry(entry(3, user("and the disks?")));
        let e = Env {
            bots: &bots,
            bot: Some("b1"),
            channel: true,
            ..env(&theme, &running)
        };
        dm.prepare(&e);
        let text = lines(&dm);
        assert!(!text.iter().any(|l| l.starts_with('▎')), "{text:?}");
        assert!(
            text.iter().any(|l| l.starts_with("◆ A chat's agent")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l == "  and the disks?"), "{text:?}");
        // A job's instructions say so, even in a plain session.
        let mut job = Transcript::default();
        job.add_entry(entry(
            1,
            EntryBody::User {
                content: vec![ContentPart::text("Brief me")],
                author: Some("job:j1".into()),
            },
        ));
        job.prepare(&env(&theme, &running));
        let text = lines(&job);
        assert!(
            text.iter().any(|l| l.starts_with("◆ Scheduled job")),
            "{text:?}"
        );
    }

    #[test]
    fn locals_and_the_last_answer() {
        let mut t = Transcript::default();
        t.add_entry(entry(1, user("question")));
        t.add_local(NoticeLevel::Error, "send failed");
        t.add_entry(entry(
            2,
            asst(
                vec![AssistantPart::Text {
                    text: "answer".into(),
                }],
                None,
            ),
        ));
        assert_eq!(t.last_seq(), 2);
        assert_eq!(t.last_assistant_text().as_deref(), Some("answer"));
    }
}
