//! SQLite persistence. Every history item is written as soon as it exists, so a
//! crash loses at most the in-flight model stream, never completed work.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tessera_proto::{
    ContentPart, DeviceClient, DeviceInfo, Entry, EntryBody, Job, SearchHit, SessionInfo,
    SessionListCursor, SessionListPage, SessionStatus,
};

use crate::util::{new_id, now_ms, sha256_hex};

// Keep the synchronous API used by tools and engine callbacks, but hand
// off a Tokio worker before contending on the database or doing disk I/O.
// Re-entry is one call deep; blocking::run restores its flag even on panic.
macro_rules! store_call {
    ($this:ident.$method:ident($($arg:expr),* $(,)?)) => {
        if crate::blocking::needed() {
            return crate::blocking::run(|| $this.$method($($arg),*));
        }
        let _store_activity = crate::activity::enter(&crate::activity::Label(
            concat!("store:", stringify!($method)),
        ));
    };
    (Self::$method:ident($($arg:expr),* $(,)?)) => {
        if crate::blocking::needed() {
            return crate::blocking::run(|| Self::$method($($arg),*));
        }
        let _store_activity = crate::activity::enter(&crate::activity::Label(
            concat!("store:", stringify!($method)),
        ));
    };
}

/// Notifications for devices and downloadable files.
pub mod notify;
pub use notify::{NewNotification, Recorded, Replace};
mod history;
/// Each run of a scheduled job.
pub mod job_runs;
#[cfg(test)]
mod stall_tests;
pub use history::HistoryPage;

/// Entries reach the full-text index in batches: one index segment per
/// batch instead of per entry (most of what an append wrote). Searches
/// index what is pending first, so they never miss anything.
const FTS_BATCH: i64 = 64;

/// The index trigger becomes batching (once): only rows already indexed
/// (`fts_upto`) are removed from the index when deleted.
const FTS_BATCHED: &str = r#"
DROP TRIGGER IF EXISTS entries_ai;
DROP TRIGGER IF EXISTS entries_ad;
CREATE TRIGGER entries_ad AFTER DELETE ON entries
WHEN old.id <= (SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'fts_upto') BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
INSERT OR REPLACE INTO meta(key, value) VALUES ('fts_upto', (SELECT coalesce(MAX(id), 0) FROM entries));
INSERT INTO meta(key, value) VALUES ('fts_batched', '1');
"#;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    info        TEXT NOT NULL,
    internal    TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    archived    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS session_history_revisions (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session_queues (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
    id          INTEGER PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    ts          INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    text        TEXT NOT NULL DEFAULT '',
    UNIQUE (session_id, seq)
);

CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    text, content='entries', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE IF NOT EXISTS tool_runs (
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    call_id     TEXT NOT NULL,
    tool        TEXT NOT NULL,
    status      TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    PRIMARY KEY (session_id, call_id)
);

CREATE TABLE IF NOT EXISTS artifacts (
    id          TEXT PRIMARY KEY,
    session_id  TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    label       TEXT NOT NULL,
    content     BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS file_snapshots (
    id          INTEGER PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    path        TEXT NOT NULL,
    content     BLOB,
    ts          INTEGER,
    after_hash  TEXT
);

CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, data TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS bots (id TEXT PRIMARY KEY, data TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS boards (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    data        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS boards_session ON boards(session_id);

CREATE TABLE IF NOT EXISTS devices (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL,
    last_seen   INTEGER
);

CREATE TABLE IF NOT EXISTS pairing_codes (code TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);

-- The engine's private per-session state (the frozen prompt and the like),
-- apart from `sessions` so the frequent info updates don't rewrite it.
CREATE TABLE IF NOT EXISTS session_state (
    id          TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    internal    TEXT NOT NULL,
    revision    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY,
    updated_at  INTEGER NOT NULL,
    data        TEXT NOT NULL
);

-- Versions of a note kept from before a change (its history).
CREATE TABLE IF NOT EXISTS note_versions (
    note_id     TEXT NOT NULL,
    version     INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    data        TEXT NOT NULL,
    PRIMARY KEY (note_id, version)
);

-- One row per model response; kept when its chat is deleted.
CREATE TABLE IF NOT EXISTS usage (
    ts          INTEGER NOT NULL,
    session_id  TEXT,
    provider    TEXT NOT NULL,
    model       TEXT NOT NULL,
    kind        TEXT NOT NULL,
    input       INTEGER NOT NULL,
    output      INTEGER NOT NULL,
    cache_read  INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    cost        REAL,
    -- The API-equivalent cost the provider reported itself (Claude Code).
    api_cost    REAL
);
CREATE INDEX IF NOT EXISTS usage_ts ON usage(ts);

-- A chat's artifacts, looked up when it is deleted.
CREATE INDEX IF NOT EXISTS artifacts_session ON artifacts(session_id);
"#;

/// Usage already in stored chats, recorded once when the `usage` table is
/// new (titles and summaries weren't kept, so only conversation turns).
const USAGE_BACKFILL: &str = r#"
INSERT INTO usage(ts, session_id, provider, model, kind, input, output, cache_read, cache_write, cost)
SELECT e.ts, e.session_id,
       coalesce(json_extract(e.body, '$.model.provider'), ''),
       coalesce(json_extract(e.body, '$.model.model'), ''),
       CASE
           WHEN json_extract(s.info, '$.internal') = 1
             OR (json_extract(s.info, '$.kind') = 'subagent' AND json_extract(s.info, '$.title') = 'Memory review')
               THEN 'memory'
           WHEN json_extract(s.info, '$.kind') IN ('subagent', 'job') THEN json_extract(s.info, '$.kind')
           ELSE 'chat'
       END,
       coalesce(json_extract(e.body, '$.usage.input'), 0),
       coalesce(json_extract(e.body, '$.usage.output'), 0),
       coalesce(json_extract(e.body, '$.usage.cache_read'), 0),
       coalesce(json_extract(e.body, '$.usage.cache_write'), 0),
       json_extract(e.body, '$.usage.cost_usd')
FROM entries e JOIN sessions s ON s.id = e.session_id
WHERE e.kind = 'assistant'
  AND coalesce(json_extract(e.body, '$.usage.input'), 0) + coalesce(json_extract(e.body, '$.usage.output'), 0)
    + coalesce(json_extract(e.body, '$.usage.cache_read'), 0) > 0
"#;

/// One model response, for the usage stats.
pub struct UsageRecord<'a> {
    pub session_id: Option<&'a str>,
    pub model: &'a tessera_proto::ModelRef,
    /// `chat`, `subagent`, `memory`, `title`, `compaction`, `job` or `other`.
    pub kind: &'a str,
    pub usage: &'a tessera_proto::Usage,
    /// What it would cost at API prices, as the provider itself reported
    /// it (Claude Code's `total_cost_usd`); used instead of the price list.
    pub api_cost: Option<f64>,
}

/// Usage summed by day, model and kind.
pub struct UsageGroup {
    pub day: String,
    pub provider: String,
    pub model: String,
    pub kind: String,
    /// `cost_usd` is the cost recorded with each request.
    pub totals: tessera_proto::UsageTotals,
    /// The requests whose provider reported their API cost: that cost, and
    /// their `input`, `output`, `cache_read` and `cache_write` tokens.
    pub reported: Option<(f64, [u64; 4])>,
}

/// A stored note. Notes kept before batch 17 have no `keep_from_agents`
/// (every agent reads them now) and an `all_bots` that meant something else:
/// it always says the opposite of `keep_from_agents` (older apps show it).
fn read_note(data: &str) -> Option<tessera_proto::Note> {
    let mut n: tessera_proto::Note = serde_json::from_str(data).ok()?;
    n.all_bots = !n.keep_from_agents;
    Some(n)
}

/// The SQL for a usage row's day (`YYYY-MM-DD`): in the device's time zone
/// as `offsets` describe it (see [`Store::usage_groups`]), else this
/// computer's. Only numbers go into it, and the engine checks them first.
fn usage_day_sql(offsets: &[tessera_proto::UtcOffset]) -> String {
    let Some((first, rest)) = offsets.split_first() else {
        return "date(ts / 1000, 'unixepoch', 'localtime')".into();
    };
    if rest.is_empty() {
        return format!(
            "date((ts + {}) / 1000, 'unixepoch')",
            i64::from(first.minutes) * 60_000
        );
    }
    // Newest first: the last change at or before the request applies.
    let mut case = String::from("CASE");
    for o in rest.iter().rev() {
        case.push_str(&format!(
            " WHEN ts >= {} THEN {}",
            o.since.min(i64::MAX as u64),
            i64::from(o.minutes) * 60_000
        ));
    }
    case.push_str(&format!(" ELSE {} END", i64::from(first.minutes) * 60_000));
    format!("date((ts + ({case})) / 1000, 'unixepoch')")
}

/// The user's Stop counts at one moment: this computer's Stop file and each
/// machine's (by id). Counts only go up and are kept where they are counted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopMarks {
    pub here: u32,
    pub machines: std::collections::BTreeMap<String, u32>,
}

impl StopMarks {
    fn is_none(&self) -> bool {
        self.here == 0 && self.machines.values().all(|n| *n == 0)
    }
}

/// A helper/bot attempt's terminal cause, never inferred from a later session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptOutcome {
    Completed,
    Failed,
    Stopped,
    TimedOut,
    Exhausted,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    /// Stable within setup, execution and handoff finalization.
    #[serde(default)]
    pub id: String,
    /// Only fixed, safe setup diagnostics, never provider error metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_cause: Option<String>,
    /// Entries strictly after this boundary belong to this attempt.
    pub from_seq: u64,
    /// Inclusive end, recorded with the terminal result (None while running).
    #[serde(default)]
    pub to_seq: Option<u64>,
    /// None while the attempt is in flight. No text from it is a final report.
    pub outcome: Option<AttemptOutcome>,
}

impl Attempt {
    /// The only pending-to-terminal transition. Cause and boundary are atomic.
    pub fn finish(
        &mut self,
        outcome: AttemptOutcome,
        end: u64,
        safe_cause: Option<String>,
    ) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        self.outcome = Some(outcome);
        self.to_seq = Some(end);
        self.safe_cause = safe_cause;
        true
    }
}
/// Engine-private per-session state persisted next to the public info.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionInternal {
    /// Monotonic engine state snapshot, including the helper allowance.
    pub revision: u64,
    /// Frozen system prompt; rebuilt only at compaction so provider prompt
    /// caches (and llama.cpp's KV cache) keep hitting.
    pub system_prompt: Option<String>,
    /// A digest of the last system prompt built, to tell when a rebuild
    /// (a setting or credential changed mid-chat) really changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_digest: Option<u64>,
    /// Date/model last shown to the agent. Changes are appended to history,
    /// not patched into the frozen system prompt (which would lose its cache).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Tool results with seq below this are shown to the model as stubs.
    pub pruned_until: u64,
    /// Ratio of provider-reported to estimated tokens.
    pub calibration: Option<f64>,
    /// Approval keys granted for the rest of the session.
    pub allowed: Vec<String>,
    pub user_turns: u32,
    /// `user_turns` when memory was last reviewed.
    #[serde(default)]
    pub reviewed_turns: u32,
    /// Entries below this seq were in a memory review already: the next
    /// one reads only what came after.
    #[serde(default)]
    pub reviewed_seq: u64,
    /// A title was set by the user or generated.
    pub titled: bool,
    /// Tool results with seq below this and larger than `demote_bytes` are
    /// shown head+tail only (set when a compaction alone could not fit).
    pub demote_before: u64,
    pub demote_bytes: usize,
    /// Reasoning from entries below this seq is no longer replayed (the
    /// cheapest way to free context: no summary needed).
    #[serde(default)]
    pub thinking_before: u64,
    /// The task ledger as the model's view shows it beside the checkpoint:
    /// updated only when the history is edited anyway (compaction, trims),
    /// so ledger updates in between don't change the start of the prompt
    /// and cost a full prompt-cache miss each. The agent sees its updates
    /// in its own `task` calls meanwhile. `None` until first set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_shown: Option<tessera_proto::TaskLedger>,
    /// Per-speaker system prompts for multi-bot channels.
    pub bot_prompts: std::collections::BTreeMap<String, String>,
    /// Injection-scan findings already reported to the user.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flagged: Vec<String>,
    /// Subagents: how they were started (definition, report format,
    /// isolation), so a continued subagent works the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent: Option<crate::agents::SubagentSpec>,
    /// Helper/bot attempt boundaries and their authoritative terminal causes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<Attempt>,
    /// Remaining allowance/deadline of the current admitted helper run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_budget: Option<crate::agents::HelperRunBudget>,
    /// A language server matched the working directory when the session was
    /// created, so it is offered the `lsp` tool (fixed for prompt caching).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lsp: bool,
    /// The Claude Code conversation this chat continues (provider
    /// `claude-code`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<crate::claude_code::Link>,
    /// In a channel, each bot on Claude Code continues its own conversation
    /// (its persona is part of it), by bot id.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub claude_code_bots: std::collections::BTreeMap<String, crate::claude_code::Link>,
    /// Secure entry/redaction invalidates opaque CLI resume links, including
    /// one a turn still in flight would otherwise save when it ends.
    pub claude_history_epoch: u64,
    /// Settings reflected by the appended context changed while a turn ran.
    pub claude_context_epoch: u64,
    /// The user's Stop counts its latest turn was accepted under, kept so a
    /// turn continued after a restart is still from before the Stops that
    /// came after it (older sessions: none, so any Stop counts against it).
    #[serde(skip_serializing_if = "StopMarks::is_none")]
    pub turn_marks: StopMarks,
    /// A value-free secure entry may refresh the bridge between turns without
    /// throwing away a safe warm CLI conversation. Never persisted.
    #[serde(skip)]
    pub claude_secure_entry: bool,
    /// Claude Code's result id for each authoritative ledger step ("": a
    /// bridged step without a CLI id). Reconciled on plan replacement and kept
    /// across resume/compaction; not a separate CLI-owned plan.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub claude_tasks: Vec<String>,
    /// Runtime Claude tasks lost on restart, even while the parent is Idle.
    /// Value-free recovery marker, not another task roster.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub claude_dependencies: bool,
    /// Open-ledger state whose completion check the agent acknowledged. Only
    /// open items/statuses count, not unrelated notes or evidence wording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_acknowledged: Option<String>,
    /// A check awaiting a reply (also keeps protocol text out of live streams).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_pending: Option<String>,
    /// Small model mode, as decided when the system prompt was built and
    /// frozen with it (`None`: off). See `profiles.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small: Option<crate::profiles::Small>,
    /// Lean start (`[agent] lean_tools`), decided and frozen the same way
    /// (`None`: off, or small model mode applies instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lean_start: Option<crate::profiles::LeanStart>,
    /// MCP servers whose tools wait until first used, decided and frozen
    /// the same way (`mcp.rs`; `None`: all up front).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_later: Option<crate::mcp::Later>,
    /// Notes the user brought into this chat in a message of theirs (a
    /// `[[link]]`, "Ask about this"): readable here also for a bot with its
    /// own memory (`engine/notes.rs note_visible`). Only the user adds to it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub note_grants: Vec<String>,
}

/// Who a token belongs to ([`Store::login`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub device: String,
    /// A machine login: it may only serve as that machine (`node_*`).
    pub machine: bool,
    /// A web browser's login: its key (an uncompressed P-256 point), which
    /// it must prove it holds each time ("Use Tessera in a web browser").
    pub browser_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    Saved,
    Stale,
    Missing,
}
pub struct Store {
    conn: Mutex<Connection>,
    /// Entries up to this rowid are in the full-text index.
    fts_upto: Mutex<i64>,
    /// Private chats live only here, in memory: nothing about them ever
    /// reaches SQLite (so not its journal, full-text index or free pages).
    private: Mutex<std::collections::HashMap<String, PrivateSession>>,
    /// Private chats' notifications: in memory only (`notify.rs`).
    private_notes: Mutex<Connection>,
    /// Histories read lately, parsed (locked after `conn`, never before).
    histories: Mutex<Histories>,
}

/// What the parsed histories kept in memory may add up to (their stored
/// size). Every agent step reads its chat's whole history; parsing it again
/// each time cost more the longer the chat ran, under the database lock
/// (about 2–3 ms a step per MB of history on a slow server). 24 MB on a
/// big machine, less on a small one, none under 1 GB (see `budget.rs`).
pub fn history_cache_budget() -> usize {
    crate::budget::get().history_cache
}

/// The parsed histories of the sessions used last, kept in step with every
/// change made through this store (appends, rewinds, rewrites, deletes).
#[derive(Default)]
struct Histories {
    map: std::collections::HashMap<String, CachedHistory>,
    /// Their stored size together.
    bytes: usize,
    /// Use counter, for dropping the least recently used.
    tick: u64,
    /// `PRAGMA data_version` when last checked. It changes when another
    /// connection (another process on this database) commits anything;
    /// then nothing kept here can be trusted.
    version: Option<i64>,
}

struct CachedHistory {
    entries: std::sync::Arc<Vec<Entry>>,
    bytes: usize,
    used: u64,
    /// A checkpoint and its tail, never usable as a complete display/recall history.
    context_only: bool,
}

impl Histories {
    /// Drop everything if another connection wrote to the database since
    /// the last look.
    fn check(&mut self, version: i64) {
        if self
            .version
            .replace(version)
            .is_some_and(|old| old != version)
        {
            self.map.clear();
            self.bytes = 0;
        }
    }

    fn get(&mut self, id: &str) -> Option<std::sync::Arc<Vec<Entry>>> {
        if self.map.get(id).is_some_and(|c| c.context_only) {
            return None;
        }
        self.get_context(id)
    }

    fn get_context(&mut self, id: &str) -> Option<std::sync::Arc<Vec<Entry>>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(id).map(|c| {
            c.used = tick;
            c.entries.clone()
        })
    }

    fn insert(&mut self, id: &str, entries: std::sync::Arc<Vec<Entry>>, bytes: usize) {
        if bytes > history_cache_budget() {
            return;
        }
        self.forget(id);
        self.tick += 1;
        self.bytes += bytes;
        self.map.insert(
            id.to_string(),
            CachedHistory {
                entries,
                bytes,
                used: self.tick,
                context_only: false,
            },
        );
        self.trim(id);
    }

    fn insert_context(
        &mut self,
        id: &str,
        entries: std::sync::Arc<Vec<Entry>>,
        bytes: usize,
        context_only: bool,
    ) {
        self.insert(id, entries, bytes);
        if let Some(c) = self.map.get_mut(id) {
            c.context_only = context_only;
        }
    }

    /// An entry was just stored: add it to a kept history (one that doesn't
    /// end right before it can't be trusted, so it goes).
    fn appended(&mut self, id: &str, entry: &Entry, bytes: usize) {
        let Some(c) = self.map.get_mut(id) else {
            return;
        };
        if (c.context_only && matches!(entry.body, EntryBody::Compaction { .. }))
            || c.entries.last().map_or(0, |e| e.seq) + 1 != entry.seq
        {
            self.forget(id);
            return;
        }
        std::sync::Arc::make_mut(&mut c.entries).push(entry.clone());
        c.bytes += bytes;
        self.bytes += bytes;
        if c.bytes > history_cache_budget() {
            self.forget(id);
        } else {
            self.trim(id);
        }
    }

    fn forget(&mut self, id: &str) {
        if let Some(c) = self.map.remove(id) {
            self.bytes -= c.bytes;
        }
    }

    /// Within budget again, dropping the least recently used (never `keep`).
    fn trim(&mut self, keep: &str) {
        while self.bytes > history_cache_budget() {
            let Some(oldest) = self
                .map
                .iter()
                .filter(|(k, _)| k.as_str() != keep)
                .min_by_key(|(_, c)| c.used)
                .map(|(k, _)| k.clone())
            else {
                return;
            };
            self.forget(&oldest);
        }
    }
}

/// Just the text of a stored note or note version.
#[derive(Deserialize)]
struct NoteText {
    #[serde(default)]
    content: String,
}

#[derive(Default)]
struct PrivateSession {
    info: Option<(SessionInfo, SessionInternal)>,
    /// The device that made it (`None`: this server itself, e.g. a test or
    /// `tessera run`): only that device sees it (`engine/private.rs`).
    owner: Option<String>,
    history_revision: u64,
    entries: Vec<Entry>,
    tool_runs: std::collections::HashSet<String>,
    artifacts: std::collections::HashMap<String, (String, Vec<u8>)>,
}

impl PrivateSession {
    /// Overwrite what we can before the memory is freed.
    fn wipe(&mut self) {
        use zeroize::Zeroize;
        fn text(parts: &mut [ContentPart]) {
            for p in parts {
                match p {
                    ContentPart::Text { text } => text.zeroize(),
                    ContentPart::Image { data, .. } => data.zeroize(),
                }
            }
        }
        for e in &mut self.entries {
            match &mut e.body {
                EntryBody::User { content, .. } | EntryBody::ToolResult { content, .. } => {
                    text(content)
                }
                EntryBody::Assistant { content, .. } => {
                    for p in content {
                        match p {
                            tessera_proto::AssistantPart::Text { text } => text.zeroize(),
                            tessera_proto::AssistantPart::Thinking { text, .. } => text.zeroize(),
                            tessera_proto::AssistantPart::ToolCall { arguments, .. } => {
                                *arguments = serde_json::Value::Null
                            }
                        }
                    }
                }
                EntryBody::Compaction { summary, .. } => summary.zeroize(),
                EntryBody::Notice { text, .. } => text.zeroize(),
            }
        }
        if let Some((info, internal)) = &mut self.info {
            info.title.zeroize();
            info.preview.zeroize();
            if let Some(p) = &mut internal.system_prompt {
                p.zeroize();
            }
        }
        for (label, content) in self.artifacts.values_mut() {
            label.zeroize();
            content.zeroize();
        }
        self.entries.clear();
        self.artifacts.clear();
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        store_call!(Self::open(path));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // Owner-only, like the rest of Tessera's data (SQLite gives its
        // journal files the database's mode).
        #[cfg(unix)]
        for suffix in ["", "-wal", "-shm"] {
            use std::os::unix::fs::PermissionsExt;
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        store_call!(Self::in_memory());
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        // The write-ahead log shrinks back after a burst instead of keeping
        // its peak size (16 MB; 4 MB on a small machine).
        conn.pragma_update(
            None,
            "journal_size_limit",
            crate::budget::get().wal_limit as i64,
        )?;
        conn.execute_batch(notify::SCHEMA)?;
        conn.execute_batch(job_runs::SCHEMA)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1')",
            [],
        )?;
        // Snapshots gained a time (to place subagents' edits on the parent's
        // timeline) and what the agent left in the file (to rewind safely).
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(file_snapshots)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        for (col, ty) in [("ts", "INTEGER"), ("after_hash", "TEXT")] {
            if !cols.iter().any(|c| c == col) {
                conn.execute(
                    &format!("ALTER TABLE file_snapshots ADD COLUMN {col} {ty}"),
                    [],
                )?;
            }
        }
        let state_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(session_state)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if !state_cols.iter().any(|c| c == "revision") {
            conn.execute(
                "ALTER TABLE session_state ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let usage_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(usage)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if !usage_cols.iter().any(|c| c == "api_cost") {
            conn.execute("ALTER TABLE usage ADD COLUMN api_cost REAL", [])?;
        }
        // Machine logins: a device paired as a machine (`kind`), or the
        // machine side of an ordinary device (`machine_token_hash`). Devices
        // from before are ordinary devices.
        let device_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(devices)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if !device_cols.iter().any(|c| c == "kind") {
            conn.execute(
                "ALTER TABLE devices ADD COLUMN kind TEXT NOT NULL DEFAULT 'device'",
                [],
            )?;
        }
        if !device_cols.iter().any(|c| c == "machine_token_hash") {
            conn.execute("ALTER TABLE devices ADD COLUMN machine_token_hash TEXT", [])?;
        }
        // What each device said it is and which computer it runs on
        // (`device_hello`, `node_hello`), and whether the user named it.
        for (col, ty) in [
            ("client", "TEXT"),
            ("computer", "TEXT"),
            ("host", "TEXT"),
            ("model", "TEXT"),
            ("named", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !device_cols.iter().any(|c| c == col) {
                conn.execute(&format!("ALTER TABLE devices ADD COLUMN {col} {ty}"), [])?;
            }
        }
        // Web browsers (`kind = 'browser'`) keep their own key's public half.
        if !device_cols.iter().any(|c| c == "device_key") {
            conn.execute("ALTER TABLE devices ADD COLUMN device_key BLOB", [])?;
        }
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS devices_machine_token ON devices(machine_token_hash)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS file_snapshots_session ON file_snapshots(session_id, seq)",
            [],
        )?;
        // A chat's subagents (asked each time a chat is opened) are found by
        // a column of their own, set once when the subagent is made: indexes
        // on `info` and `updated_at` were rewritten with the row several
        // times an agent step (measured with 2,000 chats: 0.33 ms per write
        // without them, 1.0 ms with both). The chat list sorts without one.
        let session_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(sessions)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        if !session_cols.iter().any(|c| c == "parent_id") {
            conn.execute("ALTER TABLE sessions ADD COLUMN parent_id TEXT", [])?;
        }
        // Filled in for chats made before (or by an older Tessera since).
        conn.execute_batch(
            "DROP INDEX IF EXISTS sessions_list;
             DROP INDEX IF EXISTS sessions_parent;
             UPDATE sessions SET parent_id = json_extract(info, '$.parent_id')
              WHERE parent_id IS NULL AND json_extract(info, '$.parent_id') IS NOT NULL;
             CREATE INDEX IF NOT EXISTS sessions_parent_id ON sessions(parent_id);",
        )?;
        // A maintained metadata projection. Old writers may update `info`:
        // triggers keep summaries atomic on every path, not just chat saves.
        for col in ["summary", "list_kind"] {
            if !session_cols.iter().any(|c| c == col) {
                conn.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} TEXT"), [])?;
            }
        }
        conn.execute_batch(
            "UPDATE sessions SET summary=json_remove(info,'$.task'),
                 list_kind=coalesce(json_extract(info,'$.kind'),'chat') WHERE summary IS NULL;
             CREATE TRIGGER IF NOT EXISTS sessions_summary_insert AFTER INSERT ON sessions BEGIN
               UPDATE sessions SET summary=json_remove(NEW.info,'$.task'),
                 list_kind=coalesce(json_extract(NEW.info,'$.kind'),'chat') WHERE id=NEW.id;
             END;
             CREATE TRIGGER IF NOT EXISTS sessions_summary_update AFTER UPDATE OF info ON sessions BEGIN
               UPDATE sessions SET summary=json_remove(NEW.info,'$.task'),
                 list_kind=coalesce(json_extract(NEW.info,'$.kind'),'chat') WHERE id=NEW.id;
             END;
             CREATE INDEX IF NOT EXISTS sessions_list_page ON sessions(archived,updated_at DESC,id DESC)
               WHERE list_kind IN ('chat','channel');",
        )?;
        if conn
            .query_row("SELECT 1 FROM meta WHERE key = 'fts_batched'", [], |_| {
                Ok(())
            })
            .optional()?
            .is_none()
        {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(FTS_BATCHED)?;
            tx.commit()?;
        }
        // Per-session state moved out of `sessions` (once).
        if conn
            .query_row("SELECT 1 FROM meta WHERE key = 'state_split'", [], |_| {
                Ok(())
            })
            .optional()?
            .is_none()
        {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(
                "INSERT OR IGNORE INTO session_state(id, internal) SELECT id, internal FROM sessions;
                 UPDATE sessions SET internal = '';
                 INSERT INTO meta(key, value) VALUES ('state_split', '1');",
            )?;
            tx.commit()?;
        }
        if conn
            .query_row(
                "SELECT 1 FROM meta WHERE key = 'usage_backfilled'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_none()
        {
            let tx = conn.unchecked_transaction()?;
            tx.execute(USAGE_BACKFILL, [])?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES ('usage_backfilled', '1')",
                [],
            )?;
            tx.commit()?;
        }
        let fts_upto: i64 = conn.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'fts_upto'",
            [],
            |r| r.get(0),
        )?;
        let store = Self {
            fts_upto: Mutex::new(fts_upto),
            conn: Mutex::new(conn),
            private: Mutex::new(Default::default()),
            private_notes: Mutex::new(notify::private_notes()?),
            histories: Mutex::new(Default::default()),
        };
        // Whatever was pending when the last process stopped.
        store.index_pending(&store.conn.lock())?;
        Ok(store)
    }

    pub fn is_private(&self, session_id: &str) -> bool {
        self.private.lock().contains_key(session_id)
    }

    /// A private chat's device (`Some(None)`: this server itself); `None`
    /// when `session_id` isn't a private chat.
    pub fn private_owner(&self, session_id: &str) -> Option<Option<String>> {
        self.private.lock().get(session_id).map(|p| p.owner.clone())
    }

    /// Name the device a private chat is for; nothing if it isn't one.
    pub fn set_private_owner(&self, session_id: &str, owner: Option<&str>) {
        if let Some(p) = self.private.lock().get_mut(session_id) {
            p.owner = owner.map(str::to_string);
        }
    }

    /// The private chats a device made.
    pub fn private_of(&self, device: &str) -> Vec<String> {
        self.private
            .lock()
            .iter()
            .filter(|(_, p)| p.info.is_some() && p.owner.as_deref() == Some(device))
            .map(|(id, _)| id.clone())
            .collect()
    }

    // ---- sessions -------------------------------------------------------

    pub fn put_session(&self, info: &SessionInfo, internal: &SessionInternal) -> Result<()> {
        store_call!(self.put_session(info, internal));
        let mut info = info.clone();
        info.revision = internal.revision;
        {
            let mut private = self.private.lock();
            if info.private || private.contains_key(&info.id) {
                private.entry(info.id.clone()).or_default().info =
                    Some((info.clone(), internal.clone()));
                return Ok(());
            }
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO sessions(id, info, internal, updated_at, archived, parent_id) VALUES (?1, ?2, '', ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET info = ?2, updated_at = ?3, archived = ?4",
            params![
                info.id,
                serde_json::to_string(&info)?,
                info.updated_at as i64,
                info.archived as i64,
                info.parent_id
            ],
        )?;
        tx.execute(
            "INSERT INTO session_state(id, internal, revision) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET internal = ?2, revision = ?3",
            params![info.id, serde_json::to_string(internal)?, internal.revision],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// [`put_session`](Self::put_session) when only the public info changed
    /// (the session must have been stored before).
    pub fn put_session_info(&self, info: &SessionInfo) -> Result<()> {
        store_call!(self.put_session_info(info));
        {
            let mut private = self.private.lock();
            if let Some(p) = private.get_mut(&info.id) {
                if let Some((i, _)) = &mut p.info {
                    *i = info.clone();
                }
                return Ok(());
            }
        }
        self.conn.lock().execute(
            "UPDATE sessions SET info = ?2, updated_at = ?3, archived = ?4 WHERE id = ?1",
            params![
                info.id,
                serde_json::to_string(info)?,
                info.updated_at as i64,
                info.archived as i64
            ],
        )?;
        Ok(())
    }

    /// Version the whole snapshot; a stale writer changes nothing.
    pub(crate) fn update_session(
        &self,
        info: &SessionInfo,
        internal: &SessionInternal,
        revision: u64,
    ) -> Result<SaveOutcome> {
        store_call!(self.update_session(info, internal, revision));
        let mut info = info.clone();
        info.revision = revision;
        {
            let mut private = self.private.lock();
            if let Some(p) = private.get_mut(&info.id) {
                let Some((i, state)) = &mut p.info else {
                    return Ok(SaveOutcome::Missing);
                };
                if state.revision >= revision {
                    return Ok(SaveOutcome::Stale);
                }
                *i = info.clone();
                *state = internal.clone();
                state.revision = revision;
                return Ok(SaveOutcome::Saved);
            }
            if info.private {
                return Ok(SaveOutcome::Missing);
            }
        }
        let info_json = serde_json::to_string(&info)?;
        let state_json = serde_json::to_string(internal)?;
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let old: Option<u64> = tx
            .query_row(
                "SELECT revision FROM session_state WHERE id = ?1",
                [&info.id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(old) = old else {
            return Ok(SaveOutcome::Missing);
        };
        if old >= revision {
            return Ok(SaveOutcome::Stale);
        }
        tx.execute(
            "UPDATE session_state SET internal = ?2, revision = ?3
             WHERE id = ?1 AND revision < ?3",
            params![info.id, state_json, revision],
        )?;
        tx.execute(
            "UPDATE sessions SET info = ?2, updated_at = ?3, archived = ?4 WHERE id = ?1",
            params![
                info.id,
                info_json,
                info.updated_at as i64,
                info.archived as i64
            ],
        )?;
        tx.commit()?;
        Ok(SaveOutcome::Saved)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<(SessionInfo, SessionInternal)>> {
        store_call!(self.get_session(id));
        if let Some(p) = self.private.lock().get(id) {
            return Ok(p.info.clone());
        }
        let conn = self.conn.lock();
        let row: Option<(String, String, u64)> = conn
            .query_row(
                "SELECT s.info, coalesce(t.internal, ''), coalesce(t.revision, 0) FROM sessions s
                 LEFT JOIN session_state t ON t.id = s.id WHERE s.id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(i, s, revision)| {
            let mut state: SessionInternal = serde_json::from_str(&s).unwrap_or_default();
            state.revision = revision;
            let mut info: SessionInfo = serde_json::from_str(&i)?;
            info.revision = revision;
            Ok((info, state))
        })
        .transpose()
    }

    /// The sessions whose `parent_id` is `parent` (subagents, forks).
    pub fn children_of(&self, parent: &str) -> Result<Vec<SessionInfo>> {
        store_call!(self.children_of(parent));
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT info FROM sessions WHERE parent_id = ?1")?;
        let rows = stmt.query_map([parent], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    /// Bounded keyset sidebar metadata. Pinned/open extras do not move the
    /// cursor. Only selected summaries are parsed, never historical job rows.
    pub fn list_session_page(
        &self,
        archived: bool,
        limit: u32,
        before: Option<&SessionListCursor>,
        include_ids: &[String],
    ) -> Result<SessionListPage> {
        store_call!(self.list_session_page(archived, limit, before, include_ids));
        let conn = self.conn.lock();
        let limit = limit.clamp(1, 200) as usize;
        let read = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, bool, u64)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        };
        let mut q = conn.prepare_cached(
            "SELECT s.summary,s.archived,coalesce(ss.revision,0) FROM sessions s
             LEFT JOIN session_state ss ON ss.id=s.id
             WHERE s.archived=?1 AND s.list_kind IN ('chat','channel')
               AND (?2 IS NULL OR (s.updated_at,s.id) < (?2,?3))
             ORDER BY s.updated_at DESC,s.id DESC LIMIT ?4",
        )?;
        let mut rows = q
            .query_map(
                params![
                    archived,
                    before.map(|b| b.updated_at),
                    before.map(|b| &b.id),
                    limit + 1
                ],
                read,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let decode = |(raw, archived, revision): (String, bool, u64)| -> Result<SessionInfo> {
            let mut i: SessionInfo = serde_json::from_str(&raw)?;
            i.archived = archived;
            i.revision = revision;
            if !matches!(i.status, SessionStatus::Idle | SessionStatus::Error) {
                i.status = SessionStatus::Idle;
            }
            Ok(i)
        };
        let mut sessions = rows.into_iter().map(decode).collect::<Result<Vec<_>>>()?;
        let next_cursor = if more {
            sessions.last().map(|i| SessionListCursor {
                updated_at: i.updated_at,
                id: i.id.clone(),
            })
        } else {
            None
        };
        if before.is_none() {
            let mut pinned = conn.prepare_cached(
                "SELECT s.summary,s.archived,coalesce(ss.revision,0) FROM sessions s
                 LEFT JOIN session_state ss ON ss.id=s.id
                 WHERE s.archived=?1 AND s.list_kind IN ('chat','channel')
                   AND json_extract(s.summary,'$.pinned')=1",
            )?;
            let mut extras = pinned
                .query_map([archived], read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut open = conn.prepare_cached(
                "SELECT s.summary,s.archived,coalesce(ss.revision,0) FROM sessions s
                 LEFT JOIN session_state ss ON ss.id=s.id WHERE s.id=?1",
            )?;
            for id in include_ids.iter().take(128) {
                if let Some(row) = open.query_row([id], read).optional()? {
                    extras.push(row);
                }
            }
            for row in extras {
                let i = decode(row)?;
                if !sessions.iter().any(|s| s.id == i.id) {
                    sessions.push(i);
                }
            }
        }
        drop(q);
        drop(conn);
        // Idle private chats may no longer have a loaded engine Rt. Their
        // summaries still live only in memory, and the engine filters owner.
        if before.is_none() && !archived {
            for p in self.private.lock().values() {
                if let Some((info, _)) = &p.info {
                    let mut summary = info.clone();
                    summary.task = None;
                    sessions.push(summary);
                }
            }
        }
        Ok(SessionListPage {
            sessions,
            next_cursor,
        })
    }
    pub fn list_sessions(&self, archived: bool) -> Result<Vec<SessionInfo>> {
        store_call!(self.list_sessions(archived));
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT info FROM sessions WHERE archived = ?1 ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([archived as i64], |r| r.get::<_, String>(0))?;
        let mut out: Vec<SessionInfo> = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        drop(stmt);
        drop(conn);
        if !archived {
            out.extend(
                self.private
                    .lock()
                    .values()
                    .filter_map(|p| p.info.as_ref().map(|(i, _)| i.clone())),
            );
            out.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        }
        Ok(out)
    }

    /// The chat is there (a private one only until it ends).
    pub fn session_exists(&self, id: &str) -> bool {
        if let Some(p) = self.private.lock().get(id) {
            return p.info.is_some();
        }
        self.session_in_db(id).unwrap_or(false)
    }

    fn session_in_db(&self, id: &str) -> Result<bool> {
        store_call!(self.session_in_db(id));
        Ok(self
            .conn
            .lock()
            .query_row("SELECT 1 FROM sessions WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn all_session_ids(&self) -> Result<Vec<String>> {
        store_call!(self.all_session_ids());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id FROM sessions")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        store_call!(self.delete_session(id));
        if let Some(p) = self.private.lock().get_mut(id) {
            // Keep only the audience until this process ends: broadcast events
            // already buffered for this chat must never become public.
            p.wipe();
            p.info = None;
            p.tool_runs.clear();
            return Ok(());
        }
        let conn = self.conn.lock();
        // Children (subagents, forks) keep existing but lose their parent link.
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        self.histories.lock().forget(id);
        self.fts_after_delete(&conn)
    }

    /// Add entries not yet in the full-text index (one segment for all).
    fn index_pending(&self, conn: &Connection) -> Result<()> {
        let mut upto = self.fts_upto.lock();
        let max: i64 =
            conn.query_row("SELECT coalesce(MAX(id), 0) FROM entries", [], |r| r.get(0))?;
        if max <= *upto {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO entries_fts(rowid, text) SELECT id, text FROM entries WHERE id > ?1 AND id <= ?2",
            params![*upto, max],
        )?;
        tx.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'fts_upto'",
            [max.to_string()],
        )?;
        tx.commit()?;
        *upto = max;
        Ok(())
    }

    /// After deleting entries: SQLite reuses the rowids of the newest rows,
    /// so the indexed mark never stays above what is left (rows at or below
    /// it are all indexed; deleted ones left the index through the trigger).
    fn fts_after_delete(&self, conn: &Connection) -> Result<()> {
        let mut upto = self.fts_upto.lock();
        let max: i64 =
            conn.query_row("SELECT coalesce(MAX(id), 0) FROM entries", [], |r| r.get(0))?;
        if max < *upto {
            conn.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'fts_upto'",
                [max.to_string()],
            )?;
            *upto = max;
        }
        Ok(())
    }

    // ---- entries --------------------------------------------------------

    pub(crate) fn queued(&self, id: &str) -> Result<Option<String>> {
        store_call!(self.queued(id));
        if self.is_private(id) {
            return Ok(None);
        }
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT data FROM session_queues WHERE session_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub(crate) fn put_queued(&self, id: &str, data: &str) -> Result<()> {
        store_call!(self.put_queued(id, data));
        if self.is_private(id) {
            return Ok(());
        }
        self.conn.lock().execute("INSERT INTO session_queues(session_id, data) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET data = ?2", params![id, data])?;
        Ok(())
    }

    /// Append history and remove its queued payload in the same transaction.
    pub(crate) fn append_with_queue(
        &self,
        session_id: &str,
        body: EntryBody,
        queue: Option<&str>,
    ) -> Result<Entry> {
        store_call!(self.append_with_queue(session_id, body, queue));
        if let Some(p) = self.private.lock().get_mut(session_id) {
            anyhow::ensure!(p.info.is_some(), "session was removed");
            let entry = Entry {
                seq: p.entries.len() as u64 + 1,
                ts: now_ms(),
                body,
            };
            p.entries.push(entry.clone());
            return Ok(entry);
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM entries WHERE session_id = ?1",
            [session_id],
            |r| r.get::<_, i64>(0),
        )? as u64;
        let entry = Entry {
            seq,
            ts: now_ms(),
            body,
        };
        let body_json = serde_json::to_string(&entry.body)?;
        tx.prepare_cached(
            "INSERT INTO entries(session_id, seq, ts, kind, body, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![session_id, seq as i64, entry.ts as i64, kind_name(&entry.body), body_json, searchable_text(&entry.body)])?;
        if let Some(queue) = queue {
            tx.execute("INSERT INTO session_queues(session_id, data) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET data = ?2", params![session_id, queue])?;
        }
        tx.commit()?;
        self.histories
            .lock()
            .appended(session_id, &entry, body_json.len());
        if conn.last_insert_rowid() - *self.fts_upto.lock() >= FTS_BATCH
            && let Err(e) = self.index_pending(&conn)
        {
            // The entry (and any queue consumption) already committed. A
            // maintenance failure must not look like a failed append/retry.
            tracing::warn!("history committed; deferred search indexing failed: {e}");
        }
        Ok(entry)
    }

    /// Append a history item, assigning the next sequence number.
    pub fn append(&self, session_id: &str, body: EntryBody) -> Result<Entry> {
        self.append_with_queue(session_id, body, None)
    }

    /// The seq of the session's last assistant reply (0 when none).
    pub fn last_assistant_seq(&self, session_id: &str) -> Result<u64> {
        store_call!(self.last_assistant_seq(session_id));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(p
                .entries
                .iter()
                .rev()
                .find(|e| matches!(e.body, EntryBody::Assistant { .. }))
                .map_or(0, |e| e.seq));
        }
        // Newest first, stopping at the first reply (MAX would visit every
        // row of the session to check its kind).
        let seq: Option<i64> = self
            .conn
            .lock()
            .query_row(
                "SELECT seq FROM entries WHERE session_id = ?1 AND kind = 'assistant' ORDER BY seq DESC LIMIT 1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0) as u64)
    }

    /// The history after `after_seq`. A session's whole history is kept
    /// parsed for the next read (see [`history`](Self::history)); a tail of
    /// one that isn't is read on its own.
    pub fn entries(&self, session_id: &str, after_seq: u64) -> Result<Vec<Entry>> {
        store_call!(self.entries(session_id, after_seq));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(p
                .entries
                .iter()
                .filter(|e| e.seq > after_seq)
                .cloned()
                .collect());
        }
        let conn = self.conn.lock();
        let version: i64 = conn
            .prepare_cached("PRAGMA data_version")?
            .query_row([], |r| r.get(0))?;
        let mut cache = self.histories.lock();
        cache.check(version);
        let all = match cache.get(session_id) {
            Some(all) => {
                drop(cache);
                all
            }
            None if after_seq > 0 => {
                drop(cache);
                return Ok(read_entries(&conn, session_id, after_seq)?.0);
            }
            None => {
                // Connection serialization still protects cache coherence;
                // stats and other cache readers must not wait for disk I/O.
                drop(cache);
                let (all, bytes) = read_entries(&conn, session_id, 0)?;
                let all = std::sync::Arc::new(all);
                self.histories.lock().insert(session_id, all.clone(), bytes);
                all
            }
        };
        drop(conn);
        let start = all.partition_point(|e| e.seq <= after_seq);
        Ok(all[start..].to_vec())
    }

    /// A session's whole history, shared rather than copied: what the agent
    /// loop reads on every step. Parsed once, then kept (within
    /// [`history_cache_budget`] for all sessions) and updated as entries are
    /// added.
    pub fn history(&self, session_id: &str) -> Result<std::sync::Arc<Vec<Entry>>> {
        store_call!(self.history(session_id));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(std::sync::Arc::new(p.entries.clone()));
        }
        let conn = self.conn.lock();
        let version: i64 = conn
            .prepare_cached("PRAGMA data_version")?
            .query_row([], |r| r.get(0))?;
        let mut cache = self.histories.lock();
        cache.check(version);
        if let Some(all) = cache.get(session_id) {
            return Ok(all);
        }
        drop(cache);
        let (all, bytes) = read_entries(&conn, session_id, 0)?;
        let all = std::sync::Arc::new(all);
        self.histories.lock().insert(session_id, all.clone(), bytes);
        Ok(all)
    }

    /// Chats whose parsed history is kept in memory, and their stored size.
    pub fn history_cache_stats(&self) -> (usize, usize) {
        store_call!(self.history_cache_stats());
        let cache = self.histories.lock();
        (cache.map.len(), cache.bytes)
    }

    pub fn entry(&self, session_id: &str, seq: u64) -> Result<Option<Entry>> {
        store_call!(self.entry(session_id, seq));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(p.entries.iter().find(|e| e.seq == seq).cloned());
        }
        let conn = self.conn.lock();
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT ts, body FROM entries WHERE session_id = ?1 AND seq = ?2",
                params![session_id, seq as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(ts, body)| {
            Ok(Entry {
                seq,
                ts: ts as u64,
                body: serde_json::from_str(&body)?,
            })
        })
        .transpose()
    }

    /// Kept separately from mutable session metadata: a late ordinary save
    /// must never roll this counter back after a rewind.
    pub(crate) fn history_revision(&self, id: &str) -> Result<u64> {
        store_call!(self.history_revision(id));
        if let Some(p) = self.private.lock().get(id) {
            return Ok(p.history_revision);
        }
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT revision FROM session_history_revisions WHERE session_id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Drop every history item after `after_seq` (a conversation rewind);
    /// returns how many were removed.
    pub fn truncate_entries(&self, session_id: &str, after_seq: u64) -> Result<usize> {
        self.truncate_entries_revision(session_id, after_seq)
            .map(|(removed, _)| removed)
    }

    /// The revision advances in the same transaction as the history cut.
    pub(crate) fn truncate_entries_revision(
        &self,
        session_id: &str,
        after_seq: u64,
    ) -> Result<(usize, u64)> {
        store_call!(self.truncate_entries_revision(session_id, after_seq));
        if let Some(p) = self.private.lock().get_mut(session_id) {
            anyhow::ensure!(p.info.is_some(), "session was removed");
            p.history_revision = p
                .history_revision
                .checked_add(1)
                .ok_or_else(|| anyhow!("history revision exhausted"))?;
            let before = p.entries.len();
            p.entries.retain(|e| e.seq <= after_seq);
            return Ok((before - p.entries.len(), p.history_revision));
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let revision: u64 = tx
            .query_row(
                "SELECT revision FROM session_history_revisions WHERE session_id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let revision = revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or_else(|| anyhow!("history revision exhausted"))?;
        let n = tx.execute(
            "DELETE FROM entries WHERE session_id = ?1 AND seq > ?2",
            params![session_id, after_seq as i64],
        )?;
        tx.execute(
            "INSERT INTO session_history_revisions(session_id, revision) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET revision = ?2",
            params![session_id, revision as i64],
        )?;
        tx.commit()?;
        self.histories.lock().forget(session_id);
        self.fts_after_delete(&conn)?;
        Ok((n, revision))
    }

    /// Rewrite a stored item in place (a secret moved out of the history).
    /// The old text is overwritten on disk too, not just unlinked.
    pub fn replace_entry(&self, session_id: &str, entry: &Entry) -> Result<()> {
        store_call!(self.replace_entry(session_id, entry));
        if let Some(p) = self.private.lock().get_mut(session_id) {
            if let Some(e) = p.entries.iter_mut().find(|e| e.seq == entry.seq) {
                *e = entry.clone();
            }
            return Ok(());
        }
        let conn = self.conn.lock();
        let Some((id, old)): Option<(i64, String)> = conn
            .query_row(
                "SELECT id, text FROM entries WHERE session_id = ?1 AND seq = ?2",
                params![session_id, entry.seq as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        let text = searchable_text(&entry.body);
        conn.execute_batch("PRAGMA secure_delete = ON")?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE entries SET body = ?1, text = ?2 WHERE id = ?3",
            params![serde_json::to_string(&entry.body)?, text, id],
        )?;
        if id <= *self.fts_upto.lock() {
            tx.execute(
                "INSERT INTO entries_fts(entries_fts, rowid, text) VALUES ('delete', ?1, ?2)",
                params![id, old],
            )?;
            tx.execute(
                "INSERT INTO entries_fts(rowid, text) VALUES (?1, ?2)",
                params![id, text],
            )?;
        }
        tx.commit()?;
        self.histories.lock().forget(session_id);
        conn.execute_batch("PRAGMA secure_delete = OFF")?;
        // Don't leave the old page in the write-ahead log either.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    /// Full-text search across all sessions (or one).
    pub fn search(&self, query: &str, session: Option<&str>, limit: u32) -> Result<Vec<SearchHit>> {
        store_call!(self.search(query, session, limit));
        Ok(self
            .search_since(query, session, None, limit)?
            .into_iter()
            .map(|(h, _)| h)
            .collect())
    }

    /// [`search`](Self::search) limited to entries at or after `since` (unix
    /// ms), with each hit's timestamp.
    pub fn search_since(
        &self,
        query: &str,
        session: Option<&str>,
        since: Option<u64>,
        limit: u32,
    ) -> Result<Vec<(SearchHit, u64)>> {
        store_call!(self.search_since(query, session, since, limit));
        let fts = fts_query(query);
        // Private chats are never indexed or searchable; helper sessions
        // (the memory review, which quotes the chat it reviews) are skipped.
        if fts.is_empty() || session.is_some_and(|s| self.is_private(s)) {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        self.index_pending(&conn)?;
        let mut stmt = conn.prepare(
            "SELECT e.session_id, e.seq, snippet(entries_fts, 0, '«', '»', '…', 24), s.info, e.ts
             FROM entries_fts JOIN entries e ON e.id = entries_fts.rowid JOIN sessions s ON s.id = e.session_id
             WHERE entries_fts MATCH ?1 AND (?2 IS NULL OR e.session_id = ?2) AND e.ts >= ?4
               AND coalesce(json_extract(s.info, '$.internal'), 0) = 0
             ORDER BY bm25(entries_fts) LIMIT ?3",
        )?;
        let since = since.unwrap_or(0) as i64;
        let rows = stmt.query_map(params![fts, session, limit, since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (session_id, seq, snippet, info, ts) = row?;
            let title = serde_json::from_str::<SessionInfo>(&info)
                .map(|i| i.title)
                .unwrap_or_default();
            out.push((
                SearchHit {
                    session_id,
                    session_title: title,
                    seq: seq as u64,
                    snippet,
                },
                ts as u64,
            ));
        }
        Ok(out)
    }

    /// Whether a tool call finished (`Some(true)`), began but never finished
    /// (`Some(false)`: it may have had side effects), or never started.
    pub fn tool_run_state(&self, session_id: &str, call_id: &str) -> Result<Option<bool>> {
        store_call!(self.tool_run_state(session_id, call_id));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(p.tool_runs.contains(call_id).then_some(false));
        }
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT status FROM tool_runs WHERE session_id = ?1 AND call_id = ?2",
                params![session_id, call_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| s == "finished"))
    }

    // ---- tool runs (crash recovery) --------------------------------------

    pub fn tool_started(&self, session_id: &str, call_id: &str, tool: &str) -> Result<()> {
        store_call!(self.tool_started(session_id, call_id, tool));
        if let Some(p) = self.private.lock().get_mut(session_id) {
            p.tool_runs.insert(call_id.to_string());
            return Ok(());
        }
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO tool_runs(session_id, call_id, tool, status, started_at) VALUES (?1, ?2, ?3, 'started', ?4)",
            params![session_id, call_id, tool, now_ms() as i64],
        )?;
        Ok(())
    }

    pub fn tool_finished(&self, session_id: &str, call_id: &str) -> Result<()> {
        store_call!(self.tool_finished(session_id, call_id));
        if self.is_private(session_id) {
            return Ok(());
        }
        self.conn.lock().execute(
            "UPDATE tool_runs SET status = 'finished', finished_at = ?3 WHERE session_id = ?1 AND call_id = ?2",
            params![session_id, call_id, now_ms() as i64],
        )?;
        Ok(())
    }

    /// Observed execution interval only. Unstarted/unanswered/private calls
    /// have no persisted duration; entry spacing is not execution time.
    pub fn tool_duration(&self, session_id: &str, call_id: &str) -> Result<Option<u64>> {
        store_call!(self.tool_duration(session_id, call_id));
        if self.is_private(session_id) {
            return Ok(None);
        }
        let conn = self.conn.lock();
        Ok(conn.query_row(
            "SELECT MAX(0, finished_at - started_at) FROM tool_runs WHERE session_id = ?1 AND call_id = ?2 AND finished_at IS NOT NULL",
            params![session_id, call_id], |r| r.get::<_, u64>(0),
        ).optional()?)
    }

    /// Whether a tool call began executing (and so may have had side effects).
    pub fn tool_was_started(&self, session_id: &str, call_id: &str) -> Result<bool> {
        store_call!(self.tool_was_started(session_id, call_id));
        if let Some(p) = self.private.lock().get(session_id) {
            return Ok(p.tool_runs.contains(call_id));
        }
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM tool_runs WHERE session_id = ?1 AND call_id = ?2",
                params![session_id, call_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    // ---- artifacts --------------------------------------------------------

    pub fn put_artifact(
        &self,
        session_id: Option<&str>,
        label: &str,
        content: &[u8],
    ) -> Result<String> {
        store_call!(self.put_artifact(session_id, label, content));
        let id = format!("art_{}", &new_id()[10..]);
        if let Some(sid) = session_id
            && let Some(p) = self.private.lock().get_mut(sid)
        {
            p.artifacts
                .insert(id.clone(), (label.to_string(), content.to_vec()));
            return Ok(id);
        }
        self.conn.lock().execute(
            "INSERT INTO artifacts(id, session_id, created_at, label, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, now_ms() as i64, label, content],
        )?;
        Ok(id)
    }

    pub fn artifact(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        store_call!(self.artifact(id));
        if let Some(a) = self
            .private
            .lock()
            .values()
            .find_map(|p| p.artifacts.get(id).cloned())
        {
            return Ok(Some(a));
        }
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT label, content FROM artifacts WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// The session an artifact was produced in (`None` if it has none).
    pub fn artifact_session(&self, id: &str) -> Result<Option<String>> {
        store_call!(self.artifact_session(id));
        if let Some(s) = self
            .private
            .lock()
            .iter()
            .find_map(|(sid, p)| p.artifacts.contains_key(id).then(|| sid.clone()))
        {
            return Ok(Some(s));
        }
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT session_id FROM artifacts WHERE id = ?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    // ---- file snapshots (undo) -------------------------------------------

    /// Record `path`'s content (`None`: it doesn't exist) before a tool
    /// changes it; `seq` is the assistant entry that asked for the change.
    pub fn snapshot_file(
        &self,
        session_id: &str,
        seq: u64,
        path: &str,
        content: Option<&[u8]>,
    ) -> Result<()> {
        store_call!(self.snapshot_file(session_id, seq, path, content));
        if self.is_private(session_id) {
            return Ok(()); // private chats have no file tools; never keep copies
        }
        self.conn.lock().execute(
            "INSERT INTO file_snapshots(session_id, seq, path, content, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, seq as i64, path, content, now_ms() as i64],
        )?;
        Ok(())
    }

    /// Paths awaiting a post-edit hash. Filesystem access belongs to the
    /// authorized host, never the store actor.
    pub fn open_snapshots(&self, session_id: &str, seq: u64) -> Result<Vec<(i64, String)>> {
        store_call!(self.open_snapshots(session_id, seq));
        if self.is_private(session_id) {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, path FROM file_snapshots WHERE session_id = ?1 AND seq = ?2 AND after_hash IS NULL",
        )?;
        Ok(stmt
            .query_map(params![session_id, seq as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn seal_snapshot_hash(&self, id: i64, hash: &str) -> Result<()> {
        store_call!(self.seal_snapshot_hash(id, hash));
        self.conn.lock().execute(
            "UPDATE file_snapshots SET after_hash = ?2 WHERE id = ?1 AND after_hash IS NULL",
            params![id, hash],
        )?;
        Ok(())
    }

    /// Snapshots taken in `session_id` after `after_seq`, plus those of its
    /// subagents (`children`: id and whether its undated snapshots count)
    /// taken after `after_ts`; oldest first.
    pub fn snapshots_after(
        &self,
        session_id: &str,
        after_seq: u64,
        children: &[(String, bool)],
        after_ts: u64,
    ) -> Result<Vec<FileSnapshot>> {
        store_call!(self.snapshots_after(session_id, after_seq, children, after_ts));
        let conn = self.conn.lock();
        let row = |r: &rusqlite::Row| -> rusqlite::Result<FileSnapshot> {
            Ok(FileSnapshot {
                id: r.get(0)?,
                session_id: r.get(1)?,
                seq: r.get::<_, i64>(2)? as u64,
                path: r.get(3)?,
                content: r.get(4)?,
                after_hash: r.get(5)?,
            })
        };
        const COLS: &str = "id, session_id, seq, path, content, after_hash";
        let mut out: Vec<FileSnapshot> = conn
            .prepare(&format!(
                "SELECT {COLS} FROM file_snapshots WHERE session_id = ?1 AND seq > ?2"
            ))?
            .query_map(params![session_id, after_seq as i64], row)?
            .collect::<Result<_, _>>()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLS} FROM file_snapshots WHERE session_id = ?1 AND (ts > ?2 OR (ts IS NULL AND ?3))"
        ))?;
        for (child, undated) in children {
            let rows = stmt.query_map(params![child, after_ts as i64, undated], row)?;
            for r in rows {
                out.push(r?);
            }
        }
        out.sort_by_key(|s| s.id);
        Ok(out)
    }

    /// Copy `from`'s snapshots (up to and including `up_to`) to `to` (a fork).
    pub fn copy_snapshots(&self, from: &str, to: &str, up_to: Option<u64>) -> Result<()> {
        store_call!(self.copy_snapshots(from, to, up_to));
        self.conn.lock().execute(
            "INSERT INTO file_snapshots(session_id, seq, path, content, ts, after_hash)
             SELECT ?2, seq, path, content, ts, after_hash FROM file_snapshots
             WHERE session_id = ?1 AND seq <= ?3 ORDER BY id",
            params![from, to, up_to.map_or(i64::MAX, |s| s as i64)],
        )?;
        Ok(())
    }

    /// Forget snapshots taken after `after_seq` (history rewound past them).
    pub fn delete_snapshots_after(&self, session_id: &str, after_seq: u64) -> Result<()> {
        store_call!(self.delete_snapshots_after(session_id, after_seq));
        self.conn.lock().execute(
            "DELETE FROM file_snapshots WHERE session_id = ?1 AND seq > ?2",
            params![session_id, after_seq as i64],
        )?;
        Ok(())
    }

    /// Earliest snapshot per path taken at or after `seq` (for rewinding).
    pub fn snapshots_since(
        &self,
        session_id: &str,
        seq: u64,
    ) -> Result<Vec<(String, Option<Vec<u8>>)>> {
        store_call!(self.snapshots_since(session_id, seq));
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path, content FROM file_snapshots f WHERE session_id = ?1 AND seq >= ?2
             AND id = (SELECT MIN(id) FROM file_snapshots g WHERE g.session_id = f.session_id AND g.path = f.path AND g.seq >= ?2)",
        )?;
        let rows = stmt.query_map(params![session_id, seq as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---- jobs ------------------------------------------------------------

    pub fn jobs(&self) -> Result<Vec<Job>> {
        store_call!(self.jobs());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT data FROM jobs ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    pub fn put_job(&self, job: &Job) -> Result<()> {
        store_call!(self.put_job(job));
        self.conn.lock().execute(
            "INSERT INTO jobs(id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = ?2",
            params![job.id, serde_json::to_string(job)?],
        )?;
        Ok(())
    }

    pub fn delete_job(&self, id: &str) -> Result<bool> {
        store_call!(self.delete_job(id));
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM jobs WHERE id = ?1", [id])?
            > 0)
    }

    // ---- whiteboards --------------------------------------------------------

    pub fn boards(&self, session_id: Option<&str>) -> Result<Vec<tessera_proto::Whiteboard>> {
        store_call!(self.boards(session_id));
        let conn = self.conn.lock();
        let rows: Vec<String> = match session_id {
            Some(s) => conn
                .prepare("SELECT data FROM boards WHERE session_id = ?1")?
                .query_map([s], |r| r.get(0))?
                .collect::<Result<_, _>>()?,
            None => conn
                .prepare("SELECT data FROM boards")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?,
        };
        let mut out: Vec<tessera_proto::Whiteboard> = rows
            .iter()
            .filter_map(|r| serde_json::from_str(r).ok())
            .collect();
        out.sort_by_key(|b| b.created_at);
        Ok(out)
    }

    pub fn board(&self, id: &str) -> Result<Option<tessera_proto::Whiteboard>> {
        store_call!(self.board(id));
        let conn = self.conn.lock();
        let row: Option<String> = conn
            .query_row("SELECT data FROM boards WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(row.and_then(|r| serde_json::from_str(&r).ok()))
    }

    pub fn put_board(&self, board: &tessera_proto::Whiteboard) -> Result<()> {
        store_call!(self.put_board(board));
        self.conn.lock().execute(
            "INSERT INTO boards(id, session_id, data) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET data = ?3",
            params![board.id, board.session_id, serde_json::to_string(board)?],
        )?;
        Ok(())
    }

    /// Clear only this still-current question. The conditional update also
    /// checks the stored version/content: it never overwrites a newer board.
    pub(crate) fn clear_board_ask(
        &self,
        id: &str,
        ask_id: &str,
    ) -> Result<Option<tessera_proto::Whiteboard>> {
        store_call!(self.clear_board_ask(id, ask_id));
        let conn = self.conn.lock();
        let data: Option<String> = conn
            .query_row("SELECT data FROM boards WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(data) = data else { return Ok(None) };
        let mut board: tessera_proto::Whiteboard = serde_json::from_str(&data)?;
        if !board.ask.as_ref().is_some_and(|a| a.id == ask_id) {
            return Ok(None);
        }
        board.ask = None;
        board.version += 1;
        board.updated_at = now_ms();
        let changed = conn.execute(
            "UPDATE boards SET data = ?1 WHERE id = ?2 AND data = ?3",
            params![serde_json::to_string(&board)?, id, data],
        )?;
        Ok((changed > 0).then_some(board))
    }

    pub fn delete_board(&self, id: &str) -> Result<bool> {
        store_call!(self.delete_board(id));
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM boards WHERE id = ?1", [id])?
            > 0)
    }

    // ---- notes ---------------------------------------------------------------

    /// Every note, newest first.
    pub fn notes(&self) -> Result<Vec<tessera_proto::Note>> {
        store_call!(self.notes());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT data FROM notes ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(n) = read_note(&row?) {
                out.push(n);
            }
        }
        Ok(out)
    }

    pub fn note(&self, id: &str) -> Result<Option<tessera_proto::Note>> {
        store_call!(self.note(id));
        let conn = self.conn.lock();
        let row: Option<String> = conn
            .query_row("SELECT data FROM notes WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(row.and_then(|r| read_note(&r)))
    }

    pub fn note_count(&self) -> Result<usize> {
        store_call!(self.note_count());
        let conn = self.conn.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))? as usize)
    }

    pub fn put_note(&self, note: &tessera_proto::Note) -> Result<()> {
        store_call!(self.put_note(note));
        self.conn.lock().execute(
            "INSERT INTO notes(id, updated_at, data) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET updated_at = ?2, data = ?3",
            params![
                note.id,
                note.updated_at as i64,
                serde_json::to_string(note)?
            ],
        )?;
        Ok(())
    }

    pub fn delete_note(&self, id: &str) -> Result<bool> {
        store_call!(self.delete_note(id));
        let conn = self.conn.lock();
        conn.execute("DELETE FROM note_versions WHERE note_id = ?1", [id])?;
        Ok(conn.execute("DELETE FROM notes WHERE id = ?1", [id])? > 0)
    }

    /// Keep a version of a note, then drop its oldest ones beyond `max`
    /// versions or `max_bytes` of content.
    pub fn put_note_version(
        &self,
        note_id: &str,
        v: &tessera_proto::NoteVersion,
        max: usize,
        max_bytes: usize,
    ) -> Result<()> {
        store_call!(self.put_note_version(note_id, v, max, max_bytes));
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO note_versions(note_id, version, size, data) VALUES (?1, ?2, ?3, ?4)",
            params![
                note_id,
                v.version as i64,
                v.content.len() as i64,
                serde_json::to_string(v)?
            ],
        )?;
        let mut stmt = conn.prepare(
            "SELECT version, size FROM note_versions WHERE note_id = ?1 ORDER BY version DESC",
        )?;
        let rows: Vec<(i64, i64)> = stmt
            .query_map([note_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let (mut kept, mut bytes) = (0usize, 0usize);
        for (version, size) in rows {
            bytes += size.max(0) as usize;
            kept += 1;
            // The newest one always stays.
            if kept > 1 && (kept > max || bytes > max_bytes) {
                conn.execute(
                    "DELETE FROM note_versions WHERE note_id = ?1 AND version <= ?2",
                    params![note_id, version],
                )?;
                break;
            }
        }
        Ok(())
    }

    /// A note's kept versions, newest first.
    pub fn note_versions(&self, note_id: &str) -> Result<Vec<tessera_proto::NoteVersion>> {
        store_call!(self.note_versions(note_id));
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT data FROM note_versions WHERE note_id = ?1 ORDER BY version DESC")?;
        let rows = stmt.query_map([note_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(v) = serde_json::from_str(&row?) {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Every note's text and every kept version's, one at a time (what the
    /// notes' pictures are still used by).
    pub fn each_note_text(&self, mut f: impl FnMut(&str)) -> Result<()> {
        store_call!(self.each_note_text(f));
        let conn = self.conn.lock();
        for sql in ["SELECT data FROM notes", "SELECT data FROM note_versions"] {
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let data: String = row.get(0)?;
                if let Ok(v) = serde_json::from_str::<NoteText>(&data) {
                    f(&v.content);
                }
            }
        }
        Ok(())
    }

    /// One kept version of a note; `None` for the newest.
    pub fn note_version(
        &self,
        note_id: &str,
        version: Option<u64>,
    ) -> Result<Option<tessera_proto::NoteVersion>> {
        store_call!(self.note_version(note_id, version));
        let conn = self.conn.lock();
        let row: Option<String> = match version {
            Some(v) => conn
                .query_row(
                    "SELECT data FROM note_versions WHERE note_id = ?1 AND version = ?2",
                    params![note_id, v as i64],
                    |r| r.get(0),
                )
                .optional()?,
            None => conn
                .query_row(
                    "SELECT data FROM note_versions WHERE note_id = ?1 ORDER BY version DESC LIMIT 1",
                    [note_id],
                    |r| r.get(0),
                )
                .optional()?,
        };
        Ok(row.and_then(|r| serde_json::from_str(&r).ok()))
    }

    // ---- usage ---------------------------------------------------------------

    /// Record one model response (never for private chats).
    pub fn record_usage(&self, r: &UsageRecord<'_>) -> Result<()> {
        store_call!(self.record_usage(r));
        if r.session_id.is_some_and(|s| self.is_private(s)) {
            return Ok(());
        }
        let u = r.usage;
        self.conn.lock().execute(
            "INSERT INTO usage(ts, session_id, provider, model, kind, input, output, cache_read, cache_write, cost, api_cost) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                now_ms() as i64,
                r.session_id,
                r.model.provider,
                r.model.model,
                r.kind,
                u.input as i64,
                u.output as i64,
                u.cache_read as i64,
                u.cache_write as i64,
                u.cost_usd,
                r.api_cost
            ],
        )?;
        Ok(())
    }

    /// When the first model request was recorded, if any.
    pub fn first_usage(&self) -> Result<Option<u64>> {
        store_call!(self.first_usage());
        let conn = self.conn.lock();
        let ts: Option<i64> = conn.query_row("SELECT MIN(ts) FROM usage", [], |r| r.get(0))?;
        Ok(ts.map(|t| t.max(0) as u64))
    }

    /// Usage between `from` and `to` (unix ms, inclusive), summed by day,
    /// model and kind. Days are the asking device's (`offsets`: its offset
    /// from UTC, in minutes, from each instant on, oldest first, the first
    /// also standing for everything before it), else this computer's own.
    pub fn usage_groups(
        &self,
        from: u64,
        to: u64,
        offsets: &[tessera_proto::UtcOffset],
    ) -> Result<Vec<UsageGroup>> {
        store_call!(self.usage_groups(from, to, offsets));
        let day = usage_day_sql(offsets);
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {day} AS day, provider, model, kind, COUNT(*), \
                    SUM(input), SUM(output), SUM(cache_read), SUM(cache_write), SUM(cost), SUM(api_cost), \
                    SUM(iif(api_cost IS NULL, 0, input)), SUM(iif(api_cost IS NULL, 0, output)), \
                    SUM(iif(api_cost IS NULL, 0, cache_read)), SUM(iif(api_cost IS NULL, 0, cache_write)) \
             FROM usage WHERE ts >= ?1 AND ts <= ?2 GROUP BY day, provider, model, kind ORDER BY day",
        ))?;
        let rows = stmt.query_map(params![from as i64, to as i64], |r| {
            let n = |i: usize| r.get::<_, i64>(i).map(|v| v as u64);
            Ok(UsageGroup {
                day: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                kind: r.get(3)?,
                totals: tessera_proto::UsageTotals {
                    requests: n(4)?,
                    input: n(5)?,
                    output: n(6)?,
                    cache_read: n(7)?,
                    cache_write: n(8)?,
                    cost_usd: r.get(9)?,
                    api_cost_usd: None,
                },
                reported: match r.get::<_, Option<f64>>(10)? {
                    Some(cost) => Some((cost, [n(11)?, n(12)?, n(13)?, n(14)?])),
                    None => None,
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---- bots --------------------------------------------------------------

    pub fn bots(&self) -> Result<Vec<tessera_proto::Bot>> {
        store_call!(self.bots());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT data FROM bots")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    pub fn put_bot(&self, bot: &tessera_proto::Bot) -> Result<()> {
        store_call!(self.put_bot(bot));
        self.conn.lock().execute(
            "INSERT INTO bots(id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = ?2",
            params![bot.id, serde_json::to_string(bot)?],
        )?;
        Ok(())
    }

    pub fn delete_bot(&self, id: &str) -> Result<bool> {
        store_call!(self.delete_bot(id));
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM bots WHERE id = ?1", [id])?
            > 0)
    }

    // ---- devices & pairing -------------------------------------------------

    /// Registers a device and returns (device_id, bearer token).
    pub fn add_device(&self, name: &str) -> Result<(String, String)> {
        store_call!(self.add_device(name));
        self.add_device_as(name, false)
    }

    /// Registers a device, or with `machine` a machine whose token can only
    /// serve as that machine; returns (device_id, bearer token).
    pub fn add_device_as(&self, name: &str, machine: bool) -> Result<(String, String)> {
        store_call!(self.add_device_as(name, machine));
        let id = new_id();
        let token = format!("tsr_{}", crate::util::random_token(32));
        self.conn.lock().execute(
            "INSERT INTO devices(id, name, token_hash, created_at, kind) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                name,
                sha256_hex(token.as_bytes()),
                now_ms() as i64,
                if machine { "machine" } else { "device" }
            ],
        )?;
        Ok((id, token))
    }

    /// Registers a web browser with its key (its public half, an
    /// uncompressed P-256 point); returns (device_id, bearer token). The
    /// token alone isn't enough: the browser proves it holds the key each
    /// time it connects.
    pub fn add_browser(&self, name: &str, key: &[u8]) -> Result<(String, String)> {
        store_call!(self.add_browser(name, key));
        let id = new_id();
        let token = format!("tsr_{}", crate::util::random_token(32));
        self.conn.lock().execute(
            "INSERT INTO devices(id, name, token_hash, created_at, kind, device_key) VALUES (?1, ?2, ?3, ?4, 'browser', ?5)",
            params![id, name, sha256_hex(token.as_bytes()), now_ms() as i64, key],
        )?;
        Ok((id, token))
    }

    /// The paired web browsers' ids.
    pub fn browsers(&self) -> Result<Vec<String>> {
        store_call!(self.browsers());
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id FROM devices WHERE kind = 'browser'")?;
        let ids = stmt.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<Result<Vec<_>, _>>()?)
    }

    /// Resolves a bearer token to a device id, updating `last_seen`. Only
    /// full device logins count here (not machines', nor web browsers',
    /// which need their key too); see [`Store::login`].
    pub fn authenticate(&self, token: &str) -> Result<Option<String>> {
        store_call!(self.authenticate(token));
        Ok(self
            .login(token)?
            .filter(|l| !l.machine && l.browser_key.is_none())
            .map(|l| l.device))
    }

    /// Resolves any login: a device's own token, or a machine login (a
    /// device paired as a machine, or a device's machine side). Updates
    /// `last_seen`.
    pub fn login(&self, token: &str) -> Result<Option<Login>> {
        store_call!(self.login(token));
        let conn = self.conn.lock();
        let hash = sha256_hex(token.as_bytes());
        type Found = (String, bool, Option<Vec<u8>>);
        let found: Option<Found> = conn
            .query_row(
                "SELECT id, kind = 'machine', CASE WHEN kind = 'browser' THEN coalesce(device_key, x'') END FROM devices WHERE token_hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let found = match found {
            Some(f) => Some(f),
            None => conn
                .query_row(
                    "SELECT id FROM devices WHERE machine_token_hash = ?1",
                    [&hash],
                    |r| Ok((r.get(0)?, true, None)),
                )
                .optional()?,
        };
        let Some((device, machine, browser_key)) = found else {
            return Ok(None);
        };
        conn.execute(
            "UPDATE devices SET last_seen = ?2 WHERE id = ?1",
            params![device, now_ms() as i64],
        )?;
        Ok(Some(Login {
            device,
            machine,
            browser_key,
        }))
    }

    /// A new machine login for device `id` (an ordinary device: the
    /// computer it runs on serves as a machine under its id). It replaces
    /// the one made before. `None` for an unknown device or a machine.
    pub fn machine_login(&self, id: &str) -> Result<Option<String>> {
        store_call!(self.machine_login(id));
        let token = format!("tsr_{}", crate::util::random_token(32));
        let n = self.conn.lock().execute(
            "UPDATE devices SET machine_token_hash = ?2 WHERE id = ?1 AND kind = 'device'",
            params![id, sha256_hex(token.as_bytes())],
        )?;
        Ok((n > 0).then_some(token))
    }

    pub fn devices(&self, current: Option<&str>) -> Result<Vec<DeviceInfo>> {
        store_call!(self.devices(current));
        Ok(self
            .device_rows(current)?
            .into_iter()
            .map(|(d, _)| d)
            .collect())
    }

    /// The devices with the computer key each reported (kept here: clients
    /// see only which devices share a computer, `DeviceInfo::computer`).
    /// `client` is what the device said, or `Machine` for one paired only
    /// as a machine, else `Unknown`.
    pub fn device_rows(&self, current: Option<&str>) -> Result<Vec<(DeviceInfo, Option<String>)>> {
        store_call!(self.device_rows(current));
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT d.id, d.name, d.created_at, d.last_seen, coalesce(p.notify, 1), d.kind, d.client, d.computer, d.host, d.model, d.named \
             FROM devices d LEFT JOIN device_prefs p ON p.device_id = d.id ORDER BY d.created_at, d.rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let kind: String = r.get(5)?;
            let client = match r.get::<_, Option<String>>(6)? {
                _ if kind == "machine" => DeviceClient::Machine,
                _ if kind == "browser" => DeviceClient::Browser,
                Some(c) => serde_json::from_value(serde_json::Value::String(c)).unwrap_or_default(),
                None => DeviceClient::Unknown,
            };
            Ok((
                DeviceInfo {
                    current: current == Some(id.as_str()),
                    id,
                    name: r.get(1)?,
                    created_at: r.get::<_, i64>(2)? as u64,
                    last_seen: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    notify: r.get::<_, i64>(4)? != 0,
                    online: false,
                    computer: None,
                    client,
                    host: r.get(8)?,
                    model: r.get(9)?,
                    named: r.get::<_, i64>(10)? != 0,
                    browser: kind == "browser",
                },
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Keep what a device said about itself (`device_hello`, `node_hello`).
    /// `None` leaves a field as it was; the name is never touched. A device
    /// paired only as a machine stays one whatever it says. False for an
    /// unknown device.
    pub fn report_device(
        &self,
        id: &str,
        client: Option<DeviceClient>,
        computer: Option<&str>,
        host: Option<&str>,
        model: Option<&str>,
    ) -> Result<bool> {
        store_call!(self.report_device(id, client, computer, host, model));
        let client = client
            .filter(|c| !matches!(c, DeviceClient::Unknown | DeviceClient::Machine))
            .and_then(|c| serde_json::to_value(c).ok())
            .and_then(|v| v.as_str().map(str::to_string));
        let n = self.conn.lock().execute(
            "UPDATE devices SET client = coalesce(?2, client), computer = coalesce(?3, computer), \
             host = coalesce(?4, host), model = coalesce(?5, model) WHERE id = ?1",
            params![id, client, computer, host, model],
        )?;
        Ok(n > 0)
    }

    /// Rename a device as the user asked; its name then stays as given.
    pub fn rename_device(&self, id: &str, name: &str) -> Result<bool> {
        store_call!(self.rename_device(id, name));
        let n = self.conn.lock().execute(
            "UPDATE devices SET name = ?2, named = 1 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(n > 0)
    }

    /// The device also serves as a machine (`machine_login`): signing it
    /// out would turn that machine off too.
    pub fn has_machine_login(&self, id: &str) -> Result<bool> {
        store_call!(self.has_machine_login(id));
        let n: i64 = self.conn.lock().query_row(
            "SELECT count(*) FROM devices WHERE id = ?1 AND machine_token_hash IS NOT NULL",
            [id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn revoke_device(&self, id: &str) -> Result<bool> {
        store_call!(self.revoke_device(id));
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM devices WHERE id = ?1", [id])?
            > 0)
    }

    /// Retire same-name logins issued before `keep`. The kept device must
    /// still exist: delayed cleanup never revokes a newer issuance, even
    /// when both were made in the same millisecond. Local CLI setup only.
    pub fn retire_older_devices(&self, keep: &str) -> Result<usize> {
        store_call!(self.retire_older_devices(keep));
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let kept: Option<(i64, String)> = tx
            .query_row(
                "SELECT rowid, name FROM devices WHERE id = ?1 AND kind = 'device'",
                [keep],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((row, name)) = kept else {
            // Don't include the argument: a mistaken caller might pass a
            // bearer token instead of the public device id.
            anyhow::bail!(
                "the kept local device login no longer exists; earlier logins were not retired"
            );
        };
        let retired = tx.execute(
            "DELETE FROM devices WHERE name = ?1 AND kind = 'device' AND rowid < ?2",
            params![name, row],
        )?;
        tx.commit()?;
        Ok(retired)
    }

    pub fn create_pairing_code(&self, ttl_ms: u64) -> Result<(String, u64)> {
        store_call!(self.create_pairing_code(ttl_ms));
        // 8 digits in two groups: easy to type on a phone, 10^8 space, single use,
        // short-lived and rate-limited by the server.
        let code = format!(
            "{:04}-{:04}",
            rand::random_range(0..10000),
            rand::random_range(0..10000)
        );
        let expires = now_ms() + ttl_ms;
        let conn = self.conn.lock();
        // One code at a time: encrypted pairing needs to know which code a
        // device is using without being told it.
        conn.execute("DELETE FROM pairing_codes", [])?;
        conn.execute(
            "INSERT INTO pairing_codes(code, expires_at) VALUES (?1, ?2)",
            params![code, expires as i64],
        )?;
        Ok((code, expires))
    }

    /// The current, unexpired pairing code (if any).
    pub fn active_pairing_code(&self) -> Result<Option<String>> {
        store_call!(self.active_pairing_code());
        let conn = self.conn.lock();
        Ok(conn
            .query_row("SELECT code FROM pairing_codes WHERE expires_at >= ?1 ORDER BY expires_at DESC LIMIT 1", [now_ms() as i64], |r| r.get(0))
            .optional()?)
    }

    /// Consumes a pairing code; true when it was valid.
    pub fn consume_pairing_code(&self, code: &str) -> Result<bool> {
        store_call!(self.consume_pairing_code(code));
        let normalized: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
        if normalized.len() != 8 {
            return Ok(false);
        }
        let code = format!("{}-{}", &normalized[..4], &normalized[4..]);
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM pairing_codes WHERE code = ?1 AND expires_at >= ?2",
            params![code, now_ms() as i64],
        )?;
        Ok(n > 0)
    }

    // ---- key/value ---------------------------------------------------------

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        store_call!(self.kv_get(key));
        let conn = self.conn.lock();
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        store_call!(self.kv_set(key, value));
        self.conn
            .lock()
            .execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", [key, value])?;
        Ok(())
    }

    /// Initialize once, returning the persisted winner even if another
    /// connection inserted its value meanwhile. A failed write returns no value.
    pub(crate) fn kv_get_or_insert(&self, key: &str, value: &str) -> Result<String> {
        store_call!(self.kv_get_or_insert(key, value));
        let conn = self.conn.lock();
        if let Some(saved) = conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?
        {
            return Ok(saved);
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING",
            [key, value],
        )?;
        Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))?)
    }
}

/// A file's content before a tool changed it (see [`Store::snapshot_file`]).
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub id: i64,
    pub session_id: String,
    pub seq: u64,
    pub path: String,
    /// `None`: the file did not exist.
    pub content: Option<Vec<u8>>,
    /// [`file_hash`] of the file right after the tool ran (`None` for
    /// snapshots older than this record).
    pub after_hash: Option<String>,
}

/// A session's entries after `after_seq` from the database, and their
/// stored size.
fn read_entries(
    conn: &Connection,
    session_id: &str,
    after_seq: u64,
) -> Result<(Vec<Entry>, usize)> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, ts, body FROM entries WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
    )?;
    let mut rows = stmt.query(params![session_id, after_seq as i64])?;
    let mut out = Vec::new();
    let mut bytes = 0;
    while let Some(r) = rows.next()? {
        let body = r.get_ref(2)?.as_str()?;
        bytes += body.len();
        out.push(Entry {
            seq: r.get::<_, i64>(0)? as u64,
            ts: r.get::<_, i64>(1)? as u64,
            body: serde_json::from_str(body)?,
        });
    }
    Ok((out, bytes))
}

fn kind_name(body: &EntryBody) -> &'static str {
    match body {
        EntryBody::User { .. } => "user",
        EntryBody::Assistant { .. } => "assistant",
        EntryBody::ToolResult { .. } => "tool_result",
        EntryBody::Compaction { .. } => "compaction",
        EntryBody::Notice { .. } => "notice",
    }
}

/// Text indexed for search. Tool results are indexed (truncated) so the agent
/// can recall earlier outputs after compaction.
fn searchable_text(body: &EntryBody) -> String {
    use tessera_proto::AssistantPart;
    if crate::tools::meta::is_protocol_body(body) {
        return String::new();
    }
    match body {
        EntryBody::User { content, .. } => tessera_proto::plain_text(content),
        EntryBody::Assistant { content, .. } => content
            .iter()
            .map(|p| match p {
                AssistantPart::Text { text } => text.clone(),
                AssistantPart::ToolCall {
                    name, arguments, ..
                } => {
                    format!(
                        "{name} {}",
                        crate::util::truncate(&arguments.to_string(), 2000)
                    )
                }
                AssistantPart::Thinking { .. } => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        EntryBody::ToolResult { tool, content, .. } => {
            format!(
                "{tool}: {}",
                crate::util::truncate(&tessera_proto::plain_text(content), 8000)
            )
        }
        EntryBody::Compaction { summary, .. } => summary.clone(),
        EntryBody::Notice { text, .. } => text.clone(),
    }
}

/// Turn free text into a safe FTS5 query: each word quoted, OR-joined for
/// recall, prefix-matched.
fn fts_query(q: &str) -> String {
    q.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .filter(|w| w.len() > 1)
        .take(12)
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub fn not_found(what: &str, id: &str) -> anyhow::Error {
    anyhow!("{what} `{id}` not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_proto::*;
    include!("store/history_tests.rs");
    include!("store/session_page_tests.rs");
    include!("store/launch_perf_test.rs");

    fn info(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            revision: 0,
            title: "Test".into(),
            kind: SessionKind::Chat,
            cwd: "/tmp".into(),
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            thinking: ThinkingLevel::Medium,
            permission: PermissionMode::Default,
            status: SessionStatus::Idle,
            helper_limits: None,
            helper_queued: false,
            parent_id: None,
            bot_id: None,
            members: Vec::new(),
            created_at: 1,
            updated_at: 1,
            last_seq: 0,
            usage: Usage::default(),
            context: ContextStats {
                tokens: 0,
                window: 1000,
                compactions: 0,
                source: None,
            },
            pinned: false,
            archived: false,
            preview: String::new(),
            task: None,
            private: false,
            steps: 0,
            internal: false,
            fast: false,
            daybreak: false,
            subagents: true,
            parent_call: None,
            helper_owner: None,
            role: None,
            lean: false,
            working_for: None,
        }
    }

    #[test]
    fn out_of_order_budget_saves_keep_the_newer_session_revision() {
        for private in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(&dir.path().join("state.sqlite")).unwrap();
            let mut info = info("budget-revision");
            info.private = private;
            let mut older = SessionInternal {
                revision: 1,
                helper_budget: Some(crate::agents::HelperRunBudget {
                    max_steps: 2,
                    base: 0,
                    used: 1,
                    calls: Default::default(),
                    soft_deadline_ms: None,
                    wrapped: false,
                    closed: false,
                }),
                ..Default::default()
            };
            store.put_session(&info, &older).unwrap();
            let mut newer = older.clone();
            newer.revision = 3;
            newer.helper_budget.as_mut().unwrap().used = 2;
            older.revision = 2;
            let mut newest_info = info.clone();
            newest_info.helper_limits = Some(HelperLimits {
                at_once: Some(7),
                ..Default::default()
            });
            newest_info.steps = 2;
            info.helper_limits = Some(HelperLimits {
                at_once: Some(1),
                ..Default::default()
            });
            info.steps = 1;
            assert_eq!(
                store.update_session(&newest_info, &newer, 3).unwrap(),
                SaveOutcome::Saved
            );
            assert_eq!(
                store.update_session(&info, &older, 2).unwrap(),
                SaveOutcome::Stale
            );
            let (saved_info, _) = store.get_session(&info.id).unwrap().unwrap();
            assert_eq!(saved_info.helper_limits, newest_info.helper_limits);
            assert_eq!(saved_info.steps, 2);
            assert_eq!(saved_info.revision, 3);
            let (_, saved) = store.get_session(&info.id).unwrap().unwrap();
            assert_eq!(saved.revision, 3);
            assert_eq!(saved.helper_budget.unwrap().used, 2);
        }
    }
    #[test]
    fn daybreak_choice_defaults_off_and_survives_session_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daybreak.db");
        let mut session = info("daybreak-chat");
        let old = serde_json::to_value(&session).unwrap();
        assert!(old.get("daybreak").is_none());
        assert!(!serde_json::from_value::<SessionInfo>(old).unwrap().daybreak);
        session.daybreak = true;
        {
            let store = Store::open(&path).unwrap();
            store
                .put_session(&session, &SessionInternal::default())
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.get_session(&session.id).unwrap().unwrap().0.daybreak);
        session.daybreak = false;
        store
            .put_session(&session, &SessionInternal::default())
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(!store.get_session(&session.id).unwrap().unwrap().0.daybreak);
    }
    #[test]
    fn private_rewind_revision_stays_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.db");
        let store = Store::open(&path).unwrap();
        let mut private = info("private-rewind");
        private.private = true;
        store
            .put_session(&private, &SessionInternal::default())
            .unwrap();
        store
            .append(
                &private.id,
                EntryBody::User {
                    content: vec![ContentPart::text("private history")],
                    author: None,
                },
            )
            .unwrap();
        assert_eq!(
            store.truncate_entries_revision(&private.id, 0).unwrap(),
            (1, 1)
        );
        store
            .update_session(&private, &SessionInternal::default(), 1)
            .unwrap();
        assert_eq!(store.history_revision(&private.id).unwrap(), 1);
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT count(*) FROM session_history_revisions", [], |r| {
                    r.get::<_, u64>(0)
                })
                .unwrap(),
            0
        );
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .history_revision(&private.id)
                .unwrap(),
            0
        );
    }

    #[test]
    fn rewind_history_and_revision_commit_together() {
        let store = Store::in_memory().unwrap();
        store
            .put_session(&info("rewind"), &SessionInternal::default())
            .unwrap();
        for text in ["keep", "remove"] {
            store
                .append(
                    "rewind",
                    EntryBody::User {
                        content: vec![ContentPart::text(text)],
                        author: None,
                    },
                )
                .unwrap();
        }
        store.conn.lock().execute_batch("CREATE TRIGGER fail_revision BEFORE INSERT ON session_history_revisions BEGIN SELECT RAISE(ABORT, 'revision write failed'); END;").unwrap();
        assert!(store.truncate_entries_revision("rewind", 1).is_err());
        assert_eq!(store.entries("rewind", 0).unwrap().len(), 2);
        assert_eq!(store.history_revision("rewind").unwrap(), 0);
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER fail_revision;")
            .unwrap();
        assert_eq!(
            store.truncate_entries_revision("rewind", 1).unwrap(),
            (1, 1)
        );
        // Ordinary writes captured before the rewind cannot reset the epoch.
        store
            .update_session(&info("rewind"), &SessionInternal::default(), 1)
            .unwrap();
        store
            .put_session(&info("rewind"), &SessionInternal::default())
            .unwrap();
        assert_eq!(store.history_revision("rewind").unwrap(), 1);
    }

    #[test]
    fn indexing_failure_does_not_redeliver_committed_queue_consumption() {
        let store = Store::in_memory().unwrap();
        store
            .put_session(&info("indexing"), &SessionInternal::default())
            .unwrap();
        store.put_queued("indexing", "before").unwrap();
        store
            .conn
            .lock()
            .execute_batch("DROP TABLE entries_fts;")
            .unwrap();
        *store.fts_upto.lock() = -FTS_BATCH;
        let entry = store
            .append_with_queue(
                "indexing",
                EntryBody::User {
                    content: vec![ContentPart::text("once")],
                    author: None,
                },
                Some("after"),
            )
            .unwrap();
        assert_eq!(entry.seq, 1);
        assert_eq!(store.entries("indexing", 0).unwrap().len(), 1);
        assert_eq!(store.queued("indexing").unwrap().as_deref(), Some("after"));
    }

    #[test]
    fn queue_consumption_and_history_are_one_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let store = Store::open(&path).unwrap();
        store
            .put_session(&info("queued"), &SessionInternal::default())
            .unwrap();
        store.put_queued("queued", "before").unwrap();
        store.conn.lock().execute_batch("CREATE TRIGGER fail_queue BEFORE UPDATE ON session_queues BEGIN SELECT RAISE(ABORT, 'queue write failed'); END;").unwrap();
        let body = EntryBody::User {
            content: vec![ContentPart::text("accepted")],
            author: None,
        };
        assert!(
            store
                .append_with_queue("queued", body.clone(), Some("after"))
                .is_err()
        );
        assert!(store.entries("queued", 0).unwrap().is_empty());
        assert_eq!(store.queued("queued").unwrap().as_deref(), Some("before"));
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER fail_queue;")
            .unwrap();
        store
            .append_with_queue("queued", body, Some("after"))
            .unwrap();
        drop(store);
        let restored = Store::open(&path).unwrap();
        assert_eq!(restored.entries("queued", 0).unwrap().len(), 1);
        assert_eq!(restored.queued("queued").unwrap().as_deref(), Some("after"));
    }

    #[test]
    fn session_updates_never_insert_removed_rows() {
        let s = Store::in_memory().unwrap();
        for private in [false, true] {
            let mut info = info(if private { "private" } else { "stored" });
            info.private = private;
            let mut state = SessionInternal::default();
            s.put_session(&info, &state).unwrap();
            info.title = "updated".into();
            state.system_prompt = Some("synthetic saved prompt".into());
            state.revision += 1;
            assert_eq!(
                s.update_session(&info, &state, 1).unwrap(),
                SaveOutcome::Saved
            );
            assert_eq!(
                s.get_session(&info.id).unwrap().unwrap().1.system_prompt,
                state.system_prompt
            );
            info.title = "info only".into();
            assert_eq!(
                s.update_session(&info, &state, 2).unwrap(),
                SaveOutcome::Saved
            );
            let (saved, saved_state) = s.get_session(&info.id).unwrap().unwrap();
            assert_eq!(saved.title, "info only");
            assert_eq!(saved_state.system_prompt, state.system_prompt);
            s.delete_session(&info.id).unwrap();
            assert_eq!(
                s.update_session(&info, &state, 3).unwrap(),
                SaveOutcome::Missing
            );
            assert_eq!(
                s.update_session(&info, &state, 3).unwrap(),
                SaveOutcome::Missing
            );
            assert!(s.get_session(&info.id).unwrap().is_none());
            // Deleted private sessions retain only their event audience.
            assert_eq!(s.is_private(&info.id), private);
        }
        assert!(s.all_session_ids().unwrap().is_empty());
        let states: i64 = s
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM session_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(states, 0);
    }

    #[test]
    fn entries_search_and_devices() {
        let s = Store::in_memory().unwrap();
        s.put_session(&info("a"), &SessionInternal::default())
            .unwrap();
        let e1 = s
            .append(
                "a",
                EntryBody::User {
                    content: vec![ContentPart::text("deploy the kubernetes cluster")],
                    author: None,
                },
            )
            .unwrap();
        let e2 = s
            .append(
                "a",
                EntryBody::Notice {
                    level: NoticeLevel::Info,
                    text: "ok".into(),
                    learned: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!((e1.seq, e2.seq), (1, 2));
        assert_eq!(s.entries("a", 1).unwrap().len(), 1);
        let hits = s.search("kubernetes", None, 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("«kubernetes»"));

        let (old, _) = s.create_pairing_code(60_000).unwrap();
        let (code, _) = s.create_pairing_code(60_000).unwrap();
        assert_eq!(
            s.active_pairing_code().unwrap().as_deref(),
            Some(code.as_str())
        );
        assert!(
            old == code || !s.consume_pairing_code(&old).unwrap(),
            "a new code replaces the old one"
        );
        assert!(s.consume_pairing_code(&code.replace('-', "")).unwrap());
        assert!(s.active_pairing_code().unwrap().is_none());
        assert!(
            !s.consume_pairing_code(&code).unwrap(),
            "codes are single use"
        );
        let (dev, token) = s.add_device("phone").unwrap();
        assert_eq!(s.authenticate(&token).unwrap(), Some(dev.clone()));
        // A machine login serves under the device's id but never as the device.
        let machine = s.machine_login(&dev).unwrap().unwrap();
        assert_eq!(s.authenticate(&machine).unwrap(), None);
        let login = s.login(&machine).unwrap().unwrap();
        assert!(login.machine && login.device == dev);
        assert!(!s.login(&token).unwrap().unwrap().machine);
        let newer = s.machine_login(&dev).unwrap().unwrap();
        assert!(s.login(&machine).unwrap().is_none(), "replaced");
        assert!(s.login(&newer).unwrap().is_some());
        let (box_id, box_token) = s.add_device_as("build box", true).unwrap();
        assert_eq!(s.authenticate(&box_token).unwrap(), None);
        assert_eq!(s.login(&box_token).unwrap().unwrap().device, box_id);
        assert!(
            s.machine_login(&box_id).unwrap().is_none(),
            "a machine can't make logins"
        );
        assert!(s.revoke_device(&dev).unwrap());
        assert_eq!(s.authenticate(&token).unwrap(), None);
        assert!(
            s.login(&newer).unwrap().is_none(),
            "revoked with its device"
        );

        s.delete_session("a").unwrap();
        assert!(s.entries("a", 0).unwrap().is_empty());
    }

    #[test]
    fn local_login_retirement_keeps_newer_issuances_and_other_names() {
        let s = Store::in_memory().unwrap();
        let (_, old) = s.add_device("desktop").unwrap();
        let (keep, token) = s.add_device("desktop").unwrap();
        let (newer, newest) = s.add_device("desktop").unwrap();
        let (_, other) = s.add_device("phone").unwrap();
        // Equal timestamps (or a clock adjustment) don't change issuance order.
        s.conn
            .lock()
            .execute("UPDATE devices SET created_at = 1", [])
            .unwrap();
        assert!(
            s.authenticate(&old).unwrap().is_some(),
            "issue does not retire"
        );
        assert_eq!(s.retire_older_devices(&keep).unwrap(), 1);
        assert!(s.authenticate(&old).unwrap().is_none());
        assert!(s.authenticate(&token).unwrap().is_some());
        assert!(s.authenticate(&newest).unwrap().is_some());
        assert!(s.authenticate(&other).unwrap().is_some());
        assert_eq!(s.retire_older_devices(&keep).unwrap(), 0, "idempotent");
        assert_eq!(s.retire_older_devices(&newer).unwrap(), 1);
        assert!(
            s.retire_older_devices(&keep).is_err(),
            "late cleanup of retired login refuses"
        );
        assert!(s.authenticate(&newest).unwrap().is_some());
    }

    #[test]
    fn local_login_retirement_never_retires_a_same_name_machine() {
        let s = Store::in_memory().unwrap();
        let (machine_id, machine_token) = s.add_device_as("desktop", true).unwrap();
        let (_, old) = s.add_device("desktop").unwrap();
        let (keep, token) = s.add_device("desktop").unwrap();
        assert_eq!(s.retire_older_devices(&keep).unwrap(), 1);
        assert!(s.authenticate(&old).unwrap().is_none());
        assert!(s.authenticate(&token).unwrap().is_some());
        let login = s.login(&machine_token).unwrap().unwrap();
        assert!(login.machine && login.device == machine_id);
    }
    #[test]
    fn unknown_or_machine_kept_login_does_not_retire_anything_or_echo_input() {
        let s = Store::in_memory().unwrap();
        let (_, old) = s.add_device("desktop").unwrap();
        let (machine, _) = s.add_device_as("desktop", true).unwrap();
        for keep in ["tsr_not-a-device-id", machine.as_str()] {
            let e = s.retire_older_devices(keep).unwrap_err().to_string();
            assert!(!e.contains(keep));
            assert!(s.authenticate(&old).unwrap().is_some());
        }
    }

    #[test]
    fn retirement_failure_rolls_back_the_whole_cleanup() {
        let s = Store::in_memory().unwrap();
        let (_, old) = s.add_device("desktop").unwrap();
        let (keep, token) = s.add_device("desktop").unwrap();
        s.conn.lock().execute_batch(
            "CREATE TRIGGER refuse_retirement BEFORE DELETE ON devices BEGIN SELECT RAISE(ABORT, 'cannot retire'); END;"
        ).unwrap();
        assert!(s.retire_older_devices(&keep).is_err());
        assert!(s.authenticate(&old).unwrap().is_some());
        assert!(s.authenticate(&token).unwrap().is_some());
    }
    /// Notes kept before batch 17 (no `keep_from_agents`, `all_bots` false
    /// for "only bots I add"): every agent reads them now, and older apps
    /// see them as open to all bots.
    #[test]
    fn notes_from_before_keep_from_agents_are_readable_by_agents() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.conn
            .lock()
            .execute(
                "INSERT INTO notes(id, updated_at, data) VALUES ('nt_old', 2, ?1)",
                [r#"{"id":"nt_old","title":"Old","content":"From before.","version":3,"created_at":1,"updated_at":2,"by_user":true,"all_bots":false}"#],
            )
            .unwrap();
        let n = s.note("nt_old").unwrap().unwrap();
        assert!(!n.keep_from_agents && n.all_bots, "{n:?}");
        let all = s.notes().unwrap();
        assert!(all.len() == 1 && !all[0].keep_from_agents && all[0].all_bots);
    }

    /// The user's "October 6th hasn't happened yet": days are the device's,
    /// across midnight and the end of summer time (Chicago).
    #[test]
    fn usage_days_are_the_devices_own() {
        use tessera_proto::UtcOffset;
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        // (UTC instant, Chicago's day)
        let rows = [
            (1_791_250_200_000_i64, "2026-10-05"), // Oct 6 01:30 UTC = Oct 5 20:30 CDT
            (1_791_262_740_000, "2026-10-05"),     // 04:59 UTC = 23:59 CDT
            (1_791_262_860_000, "2026-10-06"),     // 05:01 UTC = 00:01 CDT
            (1_793_511_000_000, "2026-11-01"),     // Nov 1 05:30 UTC = 00:30 CDT
            (1_793_597_400_000, "2026-11-01"),     // Nov 2 05:30 UTC = Nov 1 23:30 CST
            (1_793_601_000_000, "2026-11-02"),     // Nov 2 06:30 UTC = 00:30 CST
        ];
        {
            let conn = s.conn.lock();
            for (ts, _) in rows {
                conn.execute(
                    "INSERT INTO usage(ts, session_id, provider, model, kind, input, output, cache_read, cache_write) \
                     VALUES (?1, 's', 'p', 'm', 'chat', 1, 0, 0, 0)",
                    [ts],
                )
                .unwrap();
            }
        }
        // Summer time ends at 07:00 UTC on Nov 1 (02:00 CDT → 01:00 CST).
        let chicago = [
            UtcOffset {
                since: 0,
                minutes: -300,
            },
            UtcOffset {
                since: 1_793_516_400_000,
                minutes: -360,
            },
        ];
        let days = |offsets: &[UtcOffset]| -> Vec<(String, u64)> {
            s.usage_groups(0, u64::MAX >> 1, offsets)
                .unwrap()
                .into_iter()
                .map(|g| (g.day, g.totals.requests))
                .collect()
        };
        let want: Vec<(String, u64)> = [
            ("2026-10-05", 2),
            ("2026-10-06", 1),
            ("2026-11-01", 2),
            ("2026-11-02", 1),
        ]
        .into_iter()
        .map(|(d, n)| (d.to_string(), n))
        .collect();
        assert_eq!(days(&chicago), want);
        // In UTC the same requests fall on other days (what the server's own zone showed).
        let utc = [UtcOffset {
            since: 0,
            minutes: 0,
        }];
        let in_utc = days(&utc);
        assert_eq!(in_utc[0], ("2026-10-06".to_string(), 3));
        // One offset alone (no change in the period) works too; ahead of UTC as well.
        let tokyo = [UtcOffset {
            since: 0,
            minutes: 540,
        }];
        assert_eq!(days(&tokyo)[0], ("2026-10-06".to_string(), 3));
        assert_eq!(
            usage_day_sql(&chicago),
            "date((ts + (CASE WHEN ts >= 1793516400000 THEN -21600000 ELSE -18000000 END)) / 1000, 'unixepoch')"
        );
    }

    #[test]
    fn usage_already_in_chats_is_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let reply = |input: u64, cost: Option<f64>| EntryBody::Assistant {
            content: Vec::new(),
            model: ModelRef {
                provider: "p".into(),
                model: "m".into(),
            },
            usage: Usage {
                input,
                // A reply stopped before it began used nothing.
                output: if input == 0 { 0 } else { 5 },
                cache_read: 0,
                cache_write: 0,
                cost_usd: cost,
            },
            stop: StopReason::Stop,
            error: None,
            author: None,
        };
        {
            let s = Store::open(&path).unwrap();
            let mut review = info("r");
            review.kind = SessionKind::Subagent;
            review.title = "Memory review".into();
            for i in [info("a"), review] {
                s.put_session(&i, &SessionInternal::default()).unwrap();
            }
            s.append("a", reply(100, Some(0.5))).unwrap();
            s.append("a", reply(0, None)).unwrap();
            s.append("r", reply(40, None)).unwrap();
        }
        // As if these chats were from before the usage table existed.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DELETE FROM usage; DELETE FROM meta WHERE key = 'usage_backfilled';")
            .unwrap();
        drop(conn);
        for _ in 0..2 {
            let s = Store::open(&path).unwrap();
            let groups = s.usage_groups(0, u64::MAX >> 1, &[]).unwrap();
            let kinds: Vec<(&str, u64, u64)> = groups
                .iter()
                .map(|g| (g.kind.as_str(), g.totals.requests, g.totals.input))
                .collect();
            assert_eq!(kinds.len(), 2, "{kinds:?}");
            assert!(
                kinds.contains(&("chat", 1, 100)) && kinds.contains(&("memory", 1, 40)),
                "{kinds:?}"
            );
            let chat = groups.iter().find(|g| g.kind == "chat").unwrap();
            assert_eq!(chat.totals.cost_usd, Some(0.5));
        }
        // New responses are recorded as they happen, never for private chats.
        let s = Store::open(&path).unwrap();
        let m = ModelRef {
            provider: "p".into(),
            model: "m".into(),
        };
        let u = Usage {
            input: 1,
            ..Default::default()
        };
        s.record_usage(&UsageRecord {
            session_id: Some("a"),
            model: &m,
            kind: "title",
            usage: &u,
            api_cost: None,
        })
        .unwrap();
        let mut p = info("secret");
        p.private = true;
        s.put_session(&p, &SessionInternal::default()).unwrap();
        s.record_usage(&UsageRecord {
            session_id: Some("secret"),
            model: &m,
            kind: "chat",
            usage: &u,
            api_cost: None,
        })
        .unwrap();
        let total: u64 = s
            .usage_groups(0, u64::MAX >> 1, &[])
            .unwrap()
            .iter()
            .map(|g| g.totals.requests)
            .sum();
        assert_eq!(total, 3);
    }

    fn said(text: &str) -> EntryBody {
        EntryBody::User {
            content: vec![ContentPart::text(text)],
            author: None,
        }
    }

    fn texts(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match &e.body {
                EntryBody::User { content, .. } => plain_text(content),
                _ => String::new(),
            })
            .collect()
    }

    /// The kept histories follow every change made through the store, and
    /// what another process writes to the database.
    #[test]
    fn kept_histories_stay_in_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let s = Store::open(&path).unwrap();
        s.put_session(&info("a"), &SessionInternal::default())
            .unwrap();
        s.put_session(&info("b"), &SessionInternal::default())
            .unwrap();
        for t in ["one", "two", "three"] {
            s.append("a", said(t)).unwrap();
        }
        let first = s.history("a").unwrap();
        assert_eq!(texts(&first), ["one", "two", "three"]);
        // Read once, then shared rather than parsed again.
        assert!(std::sync::Arc::ptr_eq(&first, &s.history("a").unwrap()));
        s.append("a", said("four")).unwrap();
        assert_eq!(
            texts(&s.history("a").unwrap()),
            ["one", "two", "three", "four"]
        );
        // What was handed out earlier doesn't change under its reader.
        assert_eq!(first.len(), 3);
        assert_eq!(texts(&s.entries("a", 2).unwrap()), ["three", "four"]);
        // A rewind.
        s.truncate_entries("a", 2).unwrap();
        assert_eq!(texts(&s.history("a").unwrap()), ["one", "two"]);
        s.append("a", said("again")).unwrap();
        assert_eq!(texts(&s.entries("a", 0).unwrap()), ["one", "two", "again"]);
        // A rewritten entry.
        s.replace_entry(
            "a",
            &Entry {
                seq: 1,
                ts: 1,
                body: said("ONE"),
            },
        )
        .unwrap();
        assert_eq!(texts(&s.history("a").unwrap())[0], "ONE");
        // Another process writing to the same database.
        let other = Store::open(&path).unwrap();
        other.append("a", said("from elsewhere")).unwrap();
        assert_eq!(
            texts(&s.history("a").unwrap()),
            ["ONE", "two", "again", "from elsewhere"]
        );
        s.append("a", said("here")).unwrap();
        assert_eq!(
            texts(&other.entries("a", 0).unwrap()).last().unwrap(),
            "here"
        );
        // A tail of a history not kept is read on its own.
        s.append("b", said("b1")).unwrap();
        s.append("b", said("b2")).unwrap();
        assert_eq!(texts(&s.entries("b", 1).unwrap()), ["b2"]);
        // A deleted session.
        s.delete_session("a").unwrap();
        assert!(s.history("a").unwrap().is_empty());
    }

    #[test]
    fn kept_histories_have_a_budget() {
        if history_cache_budget() == 0 {
            return; // a machine under 1 GB keeps none
        }
        let mut h = Histories::default();
        let big = |n: u64| {
            std::sync::Arc::new(vec![Entry {
                seq: n,
                ts: 0,
                body: said("x"),
            }])
        };
        let third = history_cache_budget() / 3 + 1;
        h.insert("a", big(1), third);
        h.insert("b", big(1), third);
        assert!(h.get("a").is_some());
        // Over budget: the least recently used goes ("b"; "a" was just read).
        h.insert("c", big(1), third);
        assert!(h.map.contains_key("a") && h.map.contains_key("c"));
        assert!(!h.map.contains_key("b"));
        assert!(h.bytes <= history_cache_budget());
        // Growing past the budget alone: not kept at all.
        h.appended(
            "c",
            &Entry {
                seq: 2,
                ts: 0,
                body: said("y"),
            },
            history_cache_budget(),
        );
        assert!(!h.map.contains_key("c"));
        // An entry that doesn't follow the kept history: it is dropped.
        h.appended(
            "a",
            &Entry {
                seq: 5,
                ts: 0,
                body: said("z"),
            },
            1,
        );
        assert!(!h.map.contains_key("a"));
        assert_eq!(h.bytes, 0);
    }
}
