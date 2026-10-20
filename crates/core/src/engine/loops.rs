//! Going in circles, and what the harness says about it: the same call again
//! and again (also when only spacing, key order or output trimming differ),
//! one code file changed over and over with nothing run in between, a reply
//! that repeats itself while it streams, and a final reply that says it is
//! about to act but calls nothing. Each gets one short note; progress (a
//! successful change, a check) resets the counts, so fix-and-test loops are
//! never punished, and tools that wait or poll by nature are exempt.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use super::verify::{Scope, Verification};
use crate::tools::ToolOutput;

/// When the loop guard speaks up. `Inner::loop_limits` picks them per
/// session (small-model profiles lower `block_after` to 3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LoopLimits {
    /// The same failing call is refused after this many failures.
    pub block_after: u32,
    /// The same failing call gets a loop warning from this many failures.
    pub warn_failed: u32,
    /// The same successful call with the same result gets a loop warning
    /// from this many times.
    pub warn_same: u32,
    /// One code file changed this many times with no command run in
    /// between gets a note (0 = never).
    pub file_edits: u32,
}

impl Default for LoopLimits {
    fn default() -> Self {
        Self {
            block_after: 5,
            warn_failed: 2,
            warn_same: 3,
            file_edits: 5,
        }
    }
}

impl LoopLimits {
    /// For small models, which loop more and recover less (Roo, Cline and
    /// OpenHands block or warn at 3; R3 §2.8). Chosen by the small-model
    /// profile in `Inner::loop_limits`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn small() -> Self {
        Self {
            block_after: 3,
            warn_failed: 2,
            warn_same: 3,
            file_edits: 4,
        }
    }

    pub fn from_config(cfg: &crate::config::AgentConfig) -> Self {
        Self {
            block_after: cfg.loop_block_after.max(2),
            file_edits: cfg.edit_warn_after,
            ..Self::default()
        }
    }
}

/// Detects tool-call loops within a turn, and keeps the turn's
/// verification bookkeeping (`verify.rs`).
pub(super) struct Guardrails {
    limits: LoopLimits,
    failures: HashMap<String, u32>,
    tool_failures: HashMap<String, u32>,
    repeats: HashMap<String, (u64, u32)>,
    /// Changes per code file since a command last ran a check over it.
    edits: HashMap<String, u32>,
    /// Files whose edit count was already pointed out (until checked).
    edit_noted: HashSet<String>,
    /// This round (since the last progress check): calls, failed calls,
    /// loop warnings given (exact repeats only: a tool failing on different
    /// inputs, e.g. one site after another refusing, is trying, not circling).
    round: (u32, u32, u32),
    pub verify: Verification,
}

/// Calls that wait or poll by nature: the same answer again ("still
/// running") isn't a loop.
fn polls(name: &str, args: &Value) -> bool {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    match name {
        "job" | "agents" => true,
        "browser" => matches!(action, "wait" | "snapshot" | "screenshot" | "tabs"),
        "machines" => matches!(action, "list" | "status"),
        _ => false,
    }
}

/// Arguments that don't change what a call does: how long to wait, how
/// much output to show.
const TRIVIAL_ARGS: &[(&str, &str)] = &[
    ("bash", "timeout"),
    ("grep", "max"),
    ("glob", "limit"),
    ("web_fetch", "max_chars"),
    ("web_search", "limit"),
    ("machines", "timeout"),
];

/// A call's arguments as the loop guard compares them: keys sorted,
/// strings trimmed with runs of spaces collapsed, trivial arguments
/// dropped, and a command's output trimming (`2>&1`, `| tail -20`) ignored.
pub(super) fn normalized(name: &str, args: &Value) -> Value {
    fn walk(v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(s.split_whitespace().collect::<Vec<_>>().join(" ")),
            Value::Array(a) => Value::Array(a.iter().map(walk).collect()),
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                let mut m = Map::new();
                for k in keys {
                    m.insert(k.clone(), walk(&o[k]));
                }
                Value::Object(m)
            }
            other => other.clone(),
        }
    }
    let mut v = walk(args);
    if let Some(o) = v.as_object_mut() {
        for (tool, key) in TRIVIAL_ARGS {
            if *tool == name {
                o.remove(*key);
            }
        }
        if matches!(name, "bash" | "machines")
            && let Some(Value::String(cmd)) = o.get_mut("command")
        {
            *cmd = command_core(cmd);
        }
    }
    v
}

/// A command line without what only trims or merges its output.
fn command_core(cmd: &str) -> String {
    let mut c = cmd.trim().trim_end_matches(';').trim().to_string();
    loop {
        let before = c.clone();
        for tail in [" 2>&1", " 2>/dev/null", " &>/dev/null"] {
            if let Some(s) = c.strip_suffix(tail) {
                c = s.trim_end().to_string();
            }
        }
        // `| head -n 20`, `| tail -50`, `| cat`, `| less`
        if let Some(i) = c.rfind('|')
            && !c[..i].ends_with('|')
        {
            let last: Vec<&str> = c[i + 1..].split_whitespace().collect();
            let trims = matches!(
                last.first(),
                Some(&"head" | &"tail" | &"cat" | &"less" | &"more")
            ) && last[1..]
                .iter()
                .all(|a| a.starts_with('-') || a.chars().all(|ch| ch.is_ascii_digit()));
            if trims {
                c = c[..i].trim_end().to_string();
            }
        }
        c = c.replace(" 2>&1 ", " ");
        if c == before {
            return c;
        }
    }
}

/// An `edit`/`write` that really changed a file (not "already applied").
fn changed(name: &str, out: &ToolOutput) -> bool {
    matches!(name, "edit" | "write")
        && !out.is_error
        && out
            .details
            .as_ref()
            .and_then(|d| d.get("unchanged"))
            .is_none()
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

impl Guardrails {
    pub fn new(limits: LoopLimits, verify: Verification) -> Self {
        Self {
            limits,
            failures: HashMap::new(),
            tool_failures: HashMap::new(),
            repeats: HashMap::new(),
            edits: HashMap::new(),
            edit_noted: HashSet::new(),
            round: (0, 0, 0),
            verify,
        }
    }

    /// The context was trimmed: earlier tool output may be gone from what
    /// the model sees, so reading it again is not a loop.
    pub fn context_trimmed(&mut self) {
        self.repeats.clear();
    }

    /// Tighter limits once small model mode is known (decided with the
    /// session's first prompt, after the turn's guard was made).
    pub fn set_limits(&mut self, limits: LoopLimits) {
        self.limits = limits;
    }

    fn signature(name: &str, args: &Value) -> String {
        format!(
            "{name}:{}",
            crate::util::sha256_hex(normalized(name, args).to_string().as_bytes())
        )
    }

    pub fn blocked(&self, name: &str, args: &Value) -> Option<String> {
        let sig = Self::signature(name, args);
        match self.failures.get(&sig) {
            Some(n) if *n >= self.limits.block_after => Some(format!(
                "Blocked: this `{name}` call (or one differing only in spacing or output trimming) has already failed {n} times. Change the approach instead of repeating it."
            )),
            _ => None,
        }
    }

    /// Loop warnings ignored again and again: stop now rather than at the
    /// next progress check (`max_steps` calls away).
    pub fn stuck(&self) -> bool {
        self.round.2 >= 6
    }

    /// Going in circles this round: many loop warnings, or most calls failing.
    pub fn spinning(&self) -> bool {
        let (calls, failed, warnings) = self.round;
        warnings >= 3 || (calls >= 10 && failed * 5 >= calls * 3)
    }

    pub fn new_round(&mut self) {
        self.round = (0, 0, 0);
    }

    /// A new user turn (a queued message started it).
    pub fn new_turn(&mut self) {
        self.verify.reset();
        self.edits.clear();
        self.edit_noted.clear();
    }

    pub fn observe(&mut self, name: &str, args: &Value, out: &ToolOutput) -> Option<String> {
        let seen = self.verify.observe(name, args, out);
        let machine_change = name == "machines"
            && !out.is_error
            && matches!(
                args.get("action").and_then(Value::as_str),
                Some("write" | "edit")
            );
        // A file changed: the same test or build failing again is a new
        // attempt, not a loop (fix, test, fix, test… must not be blocked).
        if changed(name, out) || machine_change {
            self.failures.clear();
            self.tool_failures.clear();
            self.repeats.clear();
        }
        // A command checked the work: the edit counts start again.
        if let Some(c) = seen.checked.as_ref().filter(|c| c.command) {
            let place = c.machine.as_deref();
            self.edits.retain(|k, _| {
                place != super::verify::machine_of(k)
                    || matches!(&c.scope, Scope::Files(files) if !files.contains(k))
            });
            self.edit_noted.retain(|k| self.edits.contains_key(k));
        }
        let (warning, looping) = self.check(name, args, out);
        self.round.0 += 1;
        self.round.1 += out.is_error as u32;
        self.round.2 += looping as u32;
        let mut notes: Vec<String> = warning.into_iter().collect();
        for key in &seen.changed {
            let n = self.edits.entry(key.clone()).or_default();
            *n += 1;
            if self.limits.file_edits > 0
                && *n >= self.limits.file_edits
                && self.edit_noted.insert(key.clone())
            {
                notes.push(format!(
                    "[Note: this is the {} change to {} with nothing run since. Run it or its tests now to see where it stands, or step back and reconsider the approach.]",
                    ordinal(*n),
                    self.verify.shown(key)
                ));
            }
        }
        (!notes.is_empty()).then(|| notes.join("\n"))
    }

    /// The note for the model, if any, and whether it was an exact repeat
    /// (what counts toward pausing a turn).
    fn check(&mut self, name: &str, args: &Value, out: &ToolOutput) -> (Option<String>, bool) {
        let sig = Self::signature(name, args);
        if out.is_error {
            let n = self.failures.entry(sig.clone()).or_default();
            *n += 1;
            let t = self.tool_failures.entry(name.to_string()).or_default();
            *t += 1;
            if *n >= self.limits.warn_failed {
                // The tool knew the call that would work: say it again.
                let fix = out.details.as_ref().and_then(|d| d.get("fix"));
                return (
                    Some(match fix {
                        Some(fix) => format!(
                            "[Loop warning: this call has failed {n} times the same way. Send this instead: {fix}]"
                        ),
                        None => format!(
                            "[Loop warning: this call has failed {n} times. Diagnose the cause (read the error, check paths and state) before trying again.]"
                        ),
                    }),
                    true,
                );
            }
            // A nudge once per run of failures, not a loop.
            if *t == 4 {
                return (
                    Some(format!(
                        "[Note: `{name}` has failed {t} times in a row. If the cause is the same each time, step back and reconsider the approach.]"
                    )),
                    false,
                );
            }
        } else {
            self.failures.remove(&sig);
            // Failures of a tool count while they come one after another.
            self.tool_failures.remove(name);
            let hash = {
                let mut h: u64 = 1469598103934665603;
                for b in tessera_proto::plain_text(&out.content).bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(1099511628211);
                }
                h
            };
            let e = self.repeats.entry(sig).or_insert((hash, 0));
            if e.0 == hash {
                e.1 += 1;
            } else {
                *e = (hash, 1);
            }
            if e.1 >= self.limits.warn_same && !polls(name, args) {
                return (
                    Some(format!(
                        "[Loop warning: you have made this identical `{name}` call {} times with the same result. Use what you already have.]",
                        e.1
                    )),
                    true,
                );
            }
        }
        (None, false)
    }
}

// ---- repetition in a streaming reply ---------------------------------------

/// The chunk size and count of Gemini CLI's content-loop rule.
const CHUNK: usize = 50;
const TIMES: usize = 10;
/// How far back a repetition is looked for (bytes of tracked text).
const WINDOW: usize = 12_000;

/// Watches one part of a streaming reply (its text, or its reasoning) for a
/// loop: the same 50-character chunk 10 times in a row, at a constant
/// spacing with the same text in between (an exact repetition, so lists
/// with long shared prefixes and tables don't count), outside code fences.
#[derive(Default)]
pub(super) struct RepeatWatch {
    /// The text received so far.
    full: String,
    /// Bytes of `full` already looked at.
    scanned: usize,
    in_fence: bool,
    /// The text outside code fences.
    tracked: Vec<u8>,
    /// Where each chunk (by hash) ended in `tracked`.
    seen: HashMap<u64, Vec<usize>>,
    /// The period found, once found.
    found: Option<usize>,
}

impl RepeatWatch {
    /// Add streamed text; `Some(period)` once it repeats.
    pub fn push(&mut self, delta: &str) -> Option<usize> {
        if self.found.is_some() {
            return self.found;
        }
        self.full.push_str(delta);
        while self.scanned < self.full.len() {
            let rest = &self.full[self.scanned..];
            let at_line_start =
                self.scanned == 0 || self.full.as_bytes()[self.scanned - 1] == b'\n';
            if at_line_start {
                let trimmed = rest.trim_start_matches([' ', '\t']);
                // Not enough of the line yet to tell whether it opens a fence.
                if trimmed.len() < 3 && !trimmed.contains('\n') {
                    break;
                }
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    let Some(end) = rest.find('\n') else { break };
                    self.in_fence = !self.in_fence;
                    self.scanned += end + 1;
                    continue;
                }
            }
            let end = rest.find('\n').map_or(rest.len(), |i| i + 1);
            if !self.in_fence {
                let line = rest.as_bytes()[..end].to_vec();
                for b in line {
                    self.tracked.push(b);
                    if let Some(p) = self.step() {
                        self.found = Some(p);
                        return self.found;
                    }
                }
            }
            self.scanned += end;
        }
        None
    }

    /// Look at the chunk ending at the newest byte.
    fn step(&mut self) -> Option<usize> {
        let n = self.tracked.len();
        if n < CHUNK {
            return None;
        }
        let chunk = &self.tracked[n - CHUNK..];
        let mut h: u64 = 1469598103934665603;
        for b in chunk {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        let ends = self.seen.entry(h).or_default();
        ends.retain(|e| n - e <= WINDOW);
        ends.push(n);
        if ends.len() < TIMES {
            if n.is_multiple_of(4096) {
                self.seen
                    .retain(|_, v| v.last().is_some_and(|e| n - e <= WINDOW));
            }
            return None;
        }
        let last: Vec<usize> = ends[ends.len() - TIMES..].to_vec();
        let period = last[1] - last[0];
        if period == 0 || last.windows(2).any(|w| w[1] - w[0] != period) {
            return None;
        }
        // The whole stretch repeats exactly with that period…
        let t = &self.tracked;
        let mut start = last[0] - CHUNK;
        if !(start..n - period).all(|i| t[i] == t[i + period]) {
            return None;
        }
        while start > 0 && t[start - 1] == t[start - 1 + period] {
            start -= 1;
        }
        // …over at least ten chunks' worth, not just blank space (a long
        // divider line or padding isn't a loop).
        let span = &t[start..n];
        (span.len() >= CHUNK * TIMES
            && span.iter().filter(|b| !b.is_ascii_whitespace()).count() > CHUNK * 2)
            .then_some(period)
    }
}

/// Cut `text` (which ends in a loop with `period` bytes per round) after
/// the first round of the repetition, at a line or sentence end where
/// there is one: the part that was still useful.
pub(super) fn cut_repetition(text: &str, period: usize) -> String {
    let b = text.as_bytes();
    if period == 0 || b.len() <= period {
        return text.to_string();
    }
    // Walk back over the stretch that repeats.
    let mut start = b.len() - period;
    while start > 0 && b[start - 1] == b[start - 1 + period] {
        start -= 1;
    }
    let mut keep = (start + period).min(b.len());
    while keep < b.len() && !text.is_char_boundary(keep) {
        keep += 1;
    }
    let kept = &text[..keep];
    // End at a line or sentence end inside the last round, if there is one.
    let round = &kept[crate::util::floor_char(kept, start)..];
    let cut = round
        .rfind('\n')
        .or_else(|| round.rfind(". ").map(|i| i + 1))
        .map(|i| kept.len() - round.len() + i)
        .filter(|&i| i > 0);
    match cut {
        Some(i) => kept[..i].trim_end().to_string(),
        None => kept.trim_end().to_string(),
    }
}

/// A reply cut off because it repeated itself: each text or reasoning part
/// that loops ends after its first round; calls (cut off mid-way) go.
pub(super) fn trim_repeated(content: &mut Vec<tessera_proto::AssistantPart>) {
    use tessera_proto::AssistantPart;
    for part in content.iter_mut() {
        if let AssistantPart::Text { text } | AssistantPart::Thinking { text, .. } = part {
            let mut w = RepeatWatch::default();
            if let Some(period) = w.push(text) {
                *text = cut_repetition(text, period);
            }
        }
    }
    content.retain(|p| match p {
        AssistantPart::ToolCall { .. } => false,
        AssistantPart::Text { text } => !text.trim().is_empty(),
        _ => true,
    });
}

// ---- "I'll run the tests now." and no call -----------------------------------

/// What follows "let me" / "I'll" when the model is about to act.
const ACTION_VERBS: &[&str] = &[
    "run",
    "check",
    "fix",
    "update",
    "create",
    "write",
    "read",
    "look",
    "open",
    "search",
    "find",
    "install",
    "test",
    "edit",
    "add",
    "remove",
    "delete",
    "implement",
    "start",
    "try",
    "verify",
    "inspect",
    "examine",
    "review",
    "apply",
    "modify",
    "change",
    "refactor",
    "build",
    "compile",
    "execute",
    "call",
    "fetch",
    "download",
    "make",
    "set",
    "configure",
    "rewrite",
    "replace",
    "move",
    "rename",
    "investigate",
    "debug",
    "launch",
    "list",
    "grep",
    "explore",
    "analyze",
    "analyse",
    "view",
    "go",
    "proceed",
    "continue",
    "rerun",
    "re-run",
    "correct",
    "patch",
    "adjust",
    "clean",
    "commit",
    "push",
    "deploy",
    "restart",
    "kill",
    "trace",
    "scan",
    "query",
    "load",
    "save",
    "generate",
    "convert",
    "compare",
    "measure",
    "benchmark",
    "profile",
    "dig",
    "take",
    "do",
    "now",
    "first",
    "quickly",
    "also",
    "then",
    "next",
    "actually",
];

/// A sentence with one of these is about later, or up to the user.
const NOT_NOW: &[&str] = &[
    "if you",
    "when you",
    "once you",
    "later",
    "next time",
    "follow-up",
    "follow up",
    "tomorrow",
    "if needed",
    "if necessary",
    "in the future",
];

/// Openers of an announced action, after "now"/"next"/"okay".
const INTENT: &[&str] = &[
    "let me ",
    "let's ",
    "lets ",
    "i'll ",
    "i will ",
    "i'm going to ",
    "i am going to ",
    "i'm gonna ",
    "i need to ",
    "i'll now ",
    "i'm now going to ",
    "now i'll ",
    "next i'll ",
];

/// Progress words that end with "…" or ":" ("Running the tests…").
const DOING: &[&str] = &[
    "running",
    "checking",
    "fixing",
    "updating",
    "creating",
    "writing",
    "reading",
    "looking",
    "searching",
    "installing",
    "testing",
    "editing",
    "applying",
    "building",
    "fetching",
    "opening",
    "investigating",
    "trying",
    "starting",
    "verifying",
    "adding",
    "removing",
];

/// The reply says it is about to do something (in its last sentence) and
/// isn't a question: "Let me run the tests.", "Now I'll fix the import:",
/// "Running the build…". Conservative: an action verb must follow, and
/// common closers ("Let me know if…", "I'll be here") never count.
pub(super) fn announces_action(text: &str) -> bool {
    let mut t = text.trim();
    if t.is_empty() || t.ends_with('?') {
        return false;
    }
    // A command written out instead of run: "Let me run:\n```bash\nnpm test\n```".
    if t.ends_with("```")
        && let Some(open) = t[..t.len() - 3].rfind("```")
    {
        let lang = t[open + 3..]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            lang.as_str(),
            "" | "bash" | "sh" | "shell" | "console" | "zsh" | "terminal"
        ) {
            return false;
        }
        t = t[..open].trim_end();
        if !t.ends_with(':') {
            return false;
        }
    }
    // The last sentence (a full stop followed by a space or line end; a
    // dot inside `main.py` doesn't end one).
    let body = t.trim_end_matches(['.', '!', ':', '…', ' ']);
    let ending = &t[body.len()..];
    let bytes = body.as_bytes();
    let start = (0..bytes.len())
        .rev()
        .find(|&i| {
            bytes[i] == b'\n'
                || (matches!(bytes[i], b'.' | b'!' | b'?' | b';')
                    && bytes.get(i + 1).is_some_and(|b| b.is_ascii_whitespace()))
        })
        .map_or(0, |i| i + 1);
    let last = body[start..].trim();
    let mut s = last.to_lowercase().replace(['’', '‘'], "'");
    if NOT_NOW.iter().any(|w| s.contains(w)) {
        return false;
    }
    // Leading fillers: "Now", "Next,", "Okay,", "First".
    loop {
        let before = s.len();
        for filler in [
            "now", "next", "then", "first", "okay", "ok", "alright", "great", "so", "and",
            "finally", "good",
        ] {
            if let Some(rest) = s.strip_prefix(filler)
                && rest.starts_with([' ', ','])
            {
                s = rest.trim_start_matches([',', ' ']).to_string();
            }
        }
        if s.len() == before {
            break;
        }
    }
    for opener in INTENT {
        if let Some(rest) = s.strip_prefix(opener) {
            let verb = rest
                .split(|c: char| !(c.is_alphanumeric() || c == '-'))
                .next()
                .unwrap_or("");
            return ACTION_VERBS.contains(&verb);
        }
    }
    let first = s.split_whitespace().next().unwrap_or("");
    (ending.contains('…') || ending.contains("...") || ending.ends_with(':'))
        && DOING.contains(&first)
        && s.split_whitespace().count() <= 10
}

/// How an offer starts ("Want me to also add tests?"): the work is done and
/// the question only suggests more, even when it names alternatives. Lower
/// case, apostrophes straight.
const OFFER_STARTS: &[&str] = &[
    "want me to",
    "do you want me to",
    "would you like me to",
    "would you like to have me",
    "would you like a",
    "would you like some",
    "would you like more",
    "would you like any",
    "shall i",
    "should i also",
    "need me to",
    "do you need me to",
    "can i also",
    "may i also",
    "could i also",
    "would it help if i",
    "do you want a ",
    "do you want some",
    "do you want more",
];

/// Wh-questions that are only a courtesy ("How does that look?").
const WH_COURTESY: &[&str] = &[
    "how does that",
    "how does this",
    "how's that",
    "how is that",
    "how is this",
    "how do you like",
    "how can i help",
    "what else",
    "what more",
    "what next",
    "what would you like to do next",
];

/// A question words that asks for an answer before work can go on.
const WH_WORDS: &[&str] = &[
    "which", "what", "where", "when", "who", "whom", "whose", "why", "how",
];

/// Words that ask for consent to something hard to undo, or for a go-ahead:
/// the work waits for them even when they are offered politely ("Do you want
/// me to delete or overwrite the old config?").
const CONSENT_WORDS: &[&str] = &[
    "delete",
    "deleting",
    "remove",
    "removing",
    "overwrite",
    "overwriting",
    "drop",
    "wipe",
    "erase",
    "discard",
    "reset",
    "revert",
    "rollback",
    "uninstall",
    "truncate",
    "force",
    "deploy",
    "publish",
    "release",
    "push",
    "merge",
    "proceed",
    "permission",
    "allowed",
    "authorize",
    "approve",
    "confirm",
];

/// The same, as phrases.
const CONSENT_PHRASES: &[&str] = &[
    "go ahead",
    "go on",
    "roll back",
    "ok to",
    "okay to",
    "ok if",
    "okay if",
    "is it fine",
    "is that fine",
];

/// Words of a question, lower case, without code spans and punctuation.
fn question_words(question: &str) -> Vec<String> {
    let mut plain = String::with_capacity(question.len());
    let mut code = false;
    for c in question.chars() {
        match c {
            '`' => code = !code,
            _ if code => {}
            _ => plain.push(c),
        }
    }
    plain
        .to_lowercase()
        .replace(['’', '‘'], "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether a question is clearly one the work waits for: it asks for
/// consent to something hard to undo, starts with a wh-word (but "How does
/// that look?" doesn't), or chooses between alternatives ("8080 or 9090?"),
/// except an offer that names two ("Want me to add tests or docs?"). Anything
/// else is a courtesy: "Does that sound good?", "Ready for the next step?",
/// "Thoughts?", "Anything else?", and closers in other languages. A wrong
/// guess costs little: the check note is added while the agent waits, and
/// only when changed code wasn't checked.
fn blocks_the_work(question: &str) -> bool {
    let words = question_words(question);
    if words.is_empty() {
        return false;
    }
    // "So, which one?": leading fillers don't count.
    let start = words
        .iter()
        .position(|w| {
            !matches!(
                w.as_str(),
                "and" | "also" | "so" | "then" | "now" | "ok" | "okay" | "next" | "finally"
            )
        })
        .unwrap_or(0);
    let text = words[start..].join(" ");
    // Consent is the proposed action, not a word in its object (tests for a
    // delete button, docs for deployment, etc.). Ambiguous offers still get
    // the completion/verification check.
    let action = OFFER_STARTS
        .iter()
        .chain(["should i", "can i", "may i", "could i"].iter())
        .find_map(|p| text.strip_prefix(p))
        .map(str::trim)
        .unwrap_or(&text);
    let mut action = action;
    // Only leading modifiers of the proposed action, never words in its object.
    while let Some(rest) = ["permanently ", "first ", "also ", "now ", "just "]
        .iter()
        .find_map(|p| action.strip_prefix(p))
    {
        action = rest;
    }
    if action
        .split_whitespace()
        .next()
        .is_some_and(|w| CONSENT_WORDS.contains(&w))
        || CONSENT_PHRASES.iter().any(|p| action.starts_with(p))
        || [
            "is it ok",
            "is it okay",
            "is it fine",
            "is that fine",
            "do i have your permission",
        ]
        .iter()
        .any(|p| text.starts_with(p))
    {
        return true;
    }
    if WH_COURTESY.iter().any(|c| text.starts_with(c)) {
        return false;
    }
    if words
        .get(start)
        .is_some_and(|w| WH_WORDS.contains(&w.as_str()))
    {
        return true;
    }
    if OFFER_STARTS.iter().any(|o| text.starts_with(o)) {
        return false;
    }
    words.iter().any(|w| w == "or")
}

/// The sentences of `paragraph` that end with a question mark. A mark in
/// a code span (`a ? b : c`) or inside a word (`?id=3`) ends nothing.
fn questions(paragraph: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut code = false;
    let bytes = paragraph.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'`' => code = !code,
            b'\n' if !code => start = i + 1,
            b'.' | b'!' | b';' if !code => {
                if bytes.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()) {
                    start = i + 1;
                }
            }
            b'?' if !code && bytes.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()) => {
                out.push(paragraph[start..=i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out
}

/// The answer ends with a question the user has to answer before the work
/// can go on ("Which of the two do you want?"). A courtesy question after
/// finished work ("Want me to also add tests?", "Does that sound good?")
/// doesn't count: what was changed still gets its check. Only the last
/// paragraph is read, and a real question anywhere in it counts. The end may
/// be wrapped in markdown (`**…?**`) and use the full-width mark.
pub(super) fn waits_for_user(text: &str) -> bool {
    let t = text
        .replace('？', "?")
        .trim_end_matches(|c: char| c.is_whitespace() || "*_\"')]>”’".contains(c))
        .to_string();
    if !t.ends_with('?') {
        return false;
    }
    let last = t.rsplit("\n\n").next().unwrap_or(&t);
    let asked = questions(last);
    asked.is_empty() || asked.iter().any(|q| blocks_the_work(q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerifyCheck;
    use serde_json::json;
    use std::path::PathBuf;

    fn guard(limits: LoopLimits) -> Guardrails {
        Guardrails::new(
            limits,
            Verification::new(VerifyCheck::On, PathBuf::from("/w")),
        )
    }

    fn fail(text: &str) -> ToolOutput {
        ToolOutput::error(text)
    }

    #[test]
    fn near_duplicates_share_a_signature() {
        let a = json!({"command": "cargo   test 2>&1 | tail -20", "timeout": 60});
        let b = json!({"timeout": 300, "command": "cargo test | tail -n 50"});
        assert_eq!(normalized("bash", &a), normalized("bash", &b));
        assert_eq!(
            Guardrails::signature("bash", &a),
            Guardrails::signature("bash", &b)
        );
        // Different work stays different.
        for other in [
            "cargo test -p x",
            "cargo build",
            "echo site1 refused; exit 1",
        ] {
            assert_ne!(
                normalized("bash", &a),
                normalized("bash", &json!({"command": other}))
            );
        }
        assert_ne!(
            normalized("bash", &json!({"command": "echo site1 refused; exit 1"})),
            normalized("bash", &json!({"command": "echo site2 refused; exit 1"}))
        );
        // Key order and spacing inside edits.
        assert_eq!(
            normalized(
                "edit",
                &json!({"path": "a", "replace": [{"old": "x  = 1", "new": "x = 2"}]})
            ),
            normalized(
                "edit",
                &json!({"replace": [{"new": "x = 2", "old": "x = 1"}], "path": "a"})
            )
        );
        // A grep that only asks for more matches is the same search.
        assert_eq!(
            normalized("grep", &json!({"pattern": "foo", "max": 10})),
            normalized("grep", &json!({"pattern": "foo"}))
        );
    }

    #[test]
    fn near_duplicate_failures_warn_and_block() {
        let mut g = guard(LoopLimits::default());
        let call = |i: u32| json!({"command": format!("make  test{}", if i.is_multiple_of(2) { " 2>&1" } else { "" })});
        let mut last = None;
        for i in 0..5 {
            assert!(g.blocked("bash", &call(i)).is_none());
            last = g.observe("bash", &call(i), &fail("boom"));
        }
        assert!(
            last.unwrap()
                .contains("Loop warning: this call has failed 5 times")
        );
        assert!(g.blocked("bash", &call(7)).unwrap().starts_with("Blocked"));
        // Small models: refused after 3.
        let mut s = guard(LoopLimits::small());
        for i in 0..3 {
            s.observe("bash", &call(i), &fail("boom"));
        }
        assert!(s.blocked("bash", &call(0)).is_some());
    }

    #[test]
    fn a_repeated_edit_gets_the_corrected_call_back() {
        let mut g = guard(LoopLimits::default());
        let call = json!({"path": "README.md", "replace": [{"new": "receive", "old": "recieve"}], "tag": "AF1B"});
        let fix = json!({"path": "README.md", "replace": [{"old": "recieve", "new": "receive", "all": true}]});
        let err = || ToolOutput::error("matches 2 places").with_details(json!({ "fix": fix }));
        assert_eq!(g.observe("edit", &call, &err()), None);
        let w = g.observe("edit", &call, &err()).unwrap();
        assert_eq!(
            w,
            format!(
                "[Loop warning: this call has failed 2 times the same way. Send this instead: {fix}]"
            )
        );
    }

    #[test]
    fn a_successful_change_resets_and_polling_is_exempt() {
        let mut g = guard(LoopLimits::default());
        let test = json!({"command": "false"});
        for _ in 0..4 {
            g.observe("bash", &test, &fail("x"));
        }
        g.observe(
            "write",
            &json!({"path": "fix.txt", "content": "1"}),
            &ToolOutput::text("ok"),
        );
        assert!(g.blocked("bash", &test).is_none());
        // An edit that changed nothing ("already applied") is not progress.
        for _ in 0..4 {
            g.observe("bash", &test, &fail("x"));
        }
        g.observe(
            "edit",
            &json!({"path": "fix.txt"}),
            &ToolOutput::text("already").with_details(json!({"unchanged": true})),
        );
        g.observe("bash", &test, &fail("x"));
        assert!(g.blocked("bash", &test).is_some());
        // Waiting on a job is not a loop.
        let mut g = guard(LoopLimits::default());
        for _ in 0..5 {
            assert!(
                g.observe(
                    "job",
                    &json!({"action": "wait", "id": "j"}),
                    &ToolOutput::text("running")
                )
                .is_none()
            );
        }
    }

    #[test]
    fn one_file_changed_again_and_again_gets_one_note_until_checked() {
        let mut g = guard(LoopLimits::default());
        let edit = || json!({"path": "src/a.py"});
        let ok = || ToolOutput::text("edited");
        let mut notes = Vec::new();
        for _ in 0..7 {
            if let Some(n) = g.observe("edit", &edit(), &ok()) {
                notes.push(n);
            }
        }
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("5th change to src/a.py"), "{notes:?}");
        // A test run starts the count again; a docs file is never counted.
        g.observe(
            "bash",
            &json!({"command": "pytest"}),
            &ToolOutput::text("ok").with_details(json!({"exit_code": 0})),
        );
        for _ in 0..4 {
            assert!(g.observe("edit", &edit(), &ok()).is_none());
        }
        assert!(g.observe("edit", &edit(), &ok()).is_some());
        for _ in 0..8 {
            assert!(
                g.observe("write", &json!({"path": "NOTES.md"}), &ok())
                    .is_none()
            );
        }
        // Fix, test, fix, test: never.
        let mut g = guard(LoopLimits::default());
        for _ in 0..10 {
            assert!(g.observe("edit", &edit(), &ok()).is_none());
            g.observe(
                "bash",
                &json!({"command": "python -m pytest -q"}),
                &ToolOutput::error("1 failed").with_details(json!({"exit_code": 1})),
            );
        }
    }

    fn feed(w: &mut RepeatWatch, text: &str) -> Option<usize> {
        // In small pieces, as a stream arrives.
        let mut found = None;
        let mut rest = text;
        while !rest.is_empty() {
            let mut n = 7.min(rest.len());
            while !rest.is_char_boundary(n) {
                n += 1;
            }
            found = w.push(&rest[..n]).or(found);
            rest = &rest[n..];
        }
        found
    }

    #[test]
    fn a_reply_that_repeats_itself_is_caught() {
        let intro = "I looked at the parser and found the problem in the tokenizer.\n\n";
        let round = "The fix is to check the bounds before reading the next character. ";
        let text = format!("{intro}{}", round.repeat(12));
        let mut w = RepeatWatch::default();
        let period = feed(&mut w, &text).expect("caught");
        assert_eq!(period, round.len());
        let kept = cut_repetition(&text, period);
        assert_eq!(kept, format!("{intro}{}", round.trim_end()));
        // Short tight loops too.
        let mut w = RepeatWatch::default();
        assert!(feed(&mut w, &"Wait, let me reconsider. ".repeat(40)).is_some());
    }

    #[test]
    fn lists_tables_dividers_and_code_are_not_loops() {
        // Long shared prefixes in a list.
        let list: String = (0..30)
            .map(|i| {
                format!("- /home/user/projects/tessera/crates/core/src/engine/module_{i}.rs\n")
            })
            .collect();
        assert_eq!(feed(&mut RepeatWatch::default(), &list), None);
        // A table with similar rows.
        let table: String = (0..30)
            .map(|i| {
                format!("| step {i:>2} | ok | the build finished without any warnings at all |\n")
            })
            .collect();
        assert_eq!(feed(&mut RepeatWatch::default(), &table), None);
        // A divider line.
        assert_eq!(
            feed(
                &mut RepeatWatch::default(),
                &format!("{}\n", "=".repeat(300))
            ),
            None
        );
        // The same line many times inside a code block.
        let code = format!(
            "Here it is:\n```text\n{}```\nDone.",
            "same line of output repeated in a log file\n".repeat(30)
        );
        assert_eq!(feed(&mut RepeatWatch::default(), &code), None);
        // An ordinary long answer.
        let prose: String = (0..60)
            .map(|i| {
                format!(
                    "Sentence number {i} says something different about item {}. ",
                    i * 7
                )
            })
            .collect();
        assert_eq!(feed(&mut RepeatWatch::default(), &prose), None);
    }

    #[test]
    fn announced_actions_without_a_call() {
        for t in [
            "I'll run the tests now.",
            "Let me fix the import.",
            "Now let me check the config file:",
            "The parser is wrong. Next, I'll update the tokenizer.",
            "Okay, I will rewrite the function to handle empty input.",
            "Let's run the build.",
            "Running the tests…",
            "I found it. Let me run:\n```bash\npytest -q\n```",
            "I’m going to edit main.py.",
            "I need to look at the logs.",
            "Found it in src/app.py. Let me patch main.py.",
        ] {
            assert!(announces_action(t), "{t}");
        }
        for t in [
            "Let me know if you want anything else.",
            "I'll be here if you need more help.",
            "Done. The tests pass.",
            "Should I run the tests?",
            "To install it, run:\n```bash\npip install foo\n```",
            "I fixed the bug and ran the tests; all 12 pass.",
            "Let me explain: the cache was stale.",
            "I'll leave the formatting as it is.",
            "Here is the summary:\n```text\nok\n```",
            "Running the tests is up to you.",
            "I'll run the full suite later, once you confirm.",
            "I'll update the docs in a follow-up.",
            "I'll stop here.",
            "",
        ] {
            assert!(!announces_action(t), "{t}");
        }
    }

    #[test]
    fn courtesy_questions_do_not_block_the_work_but_real_ones_do() {
        // The work is done; the question only suggests more or checks in.
        for t in [
            "Done: the parser handles empty input.\n\nWant me to also add tests?",
            "All fixed. Would you like me to update the changelog?",
            "It works. Shall I also rename the old helper?",
            "Finished. If you'd like, I can also add docs. Want me to?",
            "The build passes.\n\nAnything else you'd like me to change?",
            "I changed the default. Does that make sense?",
            "Fixed in `src/app.py`. Do you want me to run the formatter?",
            "Updated. Want me to commit this?",
            "Done! Should I also bump the version?",
            "Done.\n\n**Want me to add a test for it?**",
            // Closers and check-ins, in the words models use.
            "I changed the retry loop. Does that sound good?",
            "Added the flag. Is that what you had in mind?",
            "That is the first part. Ready for the next step?",
            "I moved the helper. Thoughts?",
            "That should do it. Did that answer your question?",
            "All done. Can I help with anything else?",
            "Fixed. How does that look?",
            "Fixed. What else can I do for you?",
            // Offers that name alternatives are still offers.
            "Done. Want me to add tests or docs for it?",
            "Done. Would you like me to write a README or a changelog entry?",
            "Done. Want me to add tests for the delete button?",
            "Done. Do you want me to document the overwrite option?",
            "Done. Shall I add tests for error loading model?",
            "Done. Want me to add a release note for the rollback fix?",
            "Done. Would you like me to add a confirm dialog test?",
            "Done. Want me to first add tests for the delete button?",
            "Done. Do you want me to permanently document the overwrite option?",
            // The full-width mark, other languages, markdown around the end.
            "修正しました。他に何かありますか？",
            "Listo. ¿Algo más?",
            "Fertig. Soll ich noch etwas ändern?",
            "Done.\n\n_Want me to also add tests?_",
            "Done. (Want me to also add tests?)",
            "Done. \"Want me to also add tests?\"",
        ] {
            assert!(!waits_for_user(t), "{t}");
        }
        // The work waits for the answer.
        for t in [
            "Which of the two do you want?",
            "I found two configs. Which one should I edit?",
            "What should the new file be called?",
            "Where should the report go?",
            "Why did you pick Postgres here?",
            "Should I delete the old data first?",
            "Do you want me to delete or overwrite the existing config?",
            "Do you want me to permanently delete the old config?",
            "Do you want me to first overwrite the existing config?",
            "Want me to just go ahead and permanently delete the old config?",
            "Is it OK to overwrite the file?",
            "Can I go ahead and push this branch?",
            "Can I go ahead and restart the production server?",
            "Do you want me to go ahead and send the email?",
            "Shall I force-push to main?",
            "Done. Should I go ahead with that change?",
            "I need your OK before changing it. Should I proceed?",
            "Do I have your permission to change the server?",
            "Which name do you prefer? Want me to also add tests?",
            "Do you want the fix in `a ? b : c` form or a helper?",
            "Is the port 8080 or 9090?",
            "So, which one?",
            "Done.\n\n**Which database should I use?**",
            "Which version should I use？",
        ] {
            assert!(waits_for_user(t), "{t}");
        }
        // Not a question at all.
        for t in [
            "Done. The tests pass.",
            "If you'd like, I can also add docs.",
            "",
        ] {
            assert!(!waits_for_user(t), "{t}");
        }
        // A mark inside a code span or a link isn't a sentence end.
        assert!(!waits_for_user(
            "Use `x ? 1 : 2` there. Open /search?q=1 to see it. Want me to also add tests?"
        ));
        // Only the last paragraph counts.
        assert!(!waits_for_user(
            "Which database should I use? I went with SQLite.\n\nDone. Want me to add tests?"
        ));
    }
}
