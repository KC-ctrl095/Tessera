//! Model profiles and small model mode.
//!
//! A profile is what Tessera does differently for a model family, kept as
//! data: the sampling values its model card recommends, its size (from the
//! id, else known), the reasoning effort its maker recommends, which past
//! reasoning to send back, which edit form suits it, how much context it
//! still works well with, and Mistral's 9-character tool call ids. Families
//! are matched on the model id and, for a llama.cpp model whose id says
//! nothing, on its chat template. Users never see any of it; `[tuning]` in
//! `config.toml` overrides it per model (`config::ModelTuning`).
//!
//! Small model mode (Auto/On/Off per model) gives a model a short prompt,
//! the core tools with flat arguments, worked examples as messages, and
//! stricter loop limits. It is decided when a session's prompt is built and
//! frozen with it (prompt caches hold); the other tools are one switch away
//! (`docs` topic `tools`). Sources for every value: `docs/SMALL_MODELS.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tessera_proto::{
    AssistantPart, ContentPart, PlanStatus, ProviderKind, SmallMode, TaskLedger, ThinkingLevel,
};

use crate::config::{ModelTuning, ReasoningReplay};
use crate::llm::{Msg, ToolSpec};

/// Sampling values sent with a request; `None` leaves a value to the server.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sampling {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub min_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub repetition_penalty: Option<f64>,
}

impl Sampling {
    /// These values, with `other`'s where it sets them.
    pub fn over(mut self, other: &Sampling) -> Sampling {
        macro_rules! take {
            ($($f:ident),*) => {$(if other.$f.is_some() { self.$f = other.$f; })*};
        }
        take!(
            temperature,
            top_p,
            top_k,
            min_p,
            presence_penalty,
            repetition_penalty
        );
        self
    }

    /// The values a user set in `[tuning]`.
    pub fn of(t: &ModelTuning) -> Sampling {
        Sampling {
            temperature: t.temperature,
            top_p: t.top_p,
            top_k: t.top_k,
            min_p: t.min_p,
            presence_penalty: t.presence_penalty,
            repetition_penalty: t.repetition_penalty,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Sampling::default()
    }
}

const NONE: Sampling = Sampling {
    temperature: None,
    top_p: None,
    top_k: None,
    min_p: None,
    presence_penalty: None,
    repetition_penalty: None,
};

/// Only a temperature.
const fn t(temperature: f64) -> Sampling {
    Sampling {
        temperature: Some(temperature),
        ..NONE
    }
}

const fn tp(temperature: f64, top_p: f64) -> Sampling {
    Sampling {
        top_p: Some(top_p),
        ..t(temperature)
    }
}

const fn tpk(temperature: f64, top_p: f64, top_k: u32) -> Sampling {
    Sampling {
        top_k: Some(top_k),
        ..tp(temperature, top_p)
    }
}

/// Qwen's cards: top_k 20 and min_p 0 throughout.
const fn qwen(temperature: f64, top_p: f64, presence: Option<f64>) -> Sampling {
    Sampling {
        min_p: Some(0.0),
        presence_penalty: presence,
        ..tpk(temperature, top_p, 20)
    }
}

/// How a model edits best (`edit` takes every form; this decides what small
/// model mode offers, and is there for other per-model choices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditForm {
    /// Tagged line edits and exact replacements.
    Lines,
    /// Exact replacements only (models that fumble line numbers).
    Replace,
    /// Trained on `apply_patch` (GPT and Codex models).
    Patch,
}

struct Family {
    name: &'static str,
    /// Lowercase parts of a model id that mean this family (first row wins).
    ids: &'static [&'static str],
    /// With thinking on, and off (or the only mode).
    thinking: Option<Sampling>,
    instruct: Option<Sampling>,
    effort: Option<ThinkingLevel>,
    replay: Option<ReasoningReplay>,
    mistral_ids: bool,
    edits: EditForm,
}

const BASE: Family = Family {
    name: "",
    ids: &[],
    thinking: None,
    instruct: None,
    effort: None,
    replay: None,
    mistral_ids: false,
    edits: EditForm::Lines,
};

/// Model cards' recommendations (`docs/SMALL_MODELS.md` has the sources).
/// More specific ids come first. Never temperature 0 for a thinking model.
const FAMILIES: &[Family] = &[
    Family {
        name: "qwen3-coder",
        ids: &["qwen3-coder", "qwen3coder"],
        instruct: Some(Sampling {
            repetition_penalty: Some(1.05),
            ..qwen(0.7, 0.8, None)
        }),
        ..BASE
    },
    // Qwen3.8 keeps its own default effort (xhigh): its card warns that
    // lower effort fails more often and retries more.
    Family {
        name: "qwen3.6",
        ids: &["qwen3.6", "qwen3.8"],
        thinking: Some(qwen(1.0, 0.95, Some(0.0))),
        instruct: Some(qwen(0.7, 0.8, Some(1.5))),
        ..BASE
    },
    Family {
        name: "qwen3.5",
        ids: &["qwen3.5"],
        thinking: Some(qwen(0.6, 0.95, Some(0.0))),
        instruct: Some(qwen(0.7, 0.8, Some(1.5))),
        ..BASE
    },
    Family {
        name: "qwen3",
        ids: &["qwen3", "qwq"],
        thinking: Some(qwen(0.6, 0.95, None)),
        instruct: Some(qwen(0.7, 0.8, None)),
        replay: Some(ReasoningReplay::Turn),
        ..BASE
    },
    Family {
        name: "qwen2.5",
        ids: &["qwen2.5"],
        instruct: Some(Sampling {
            repetition_penalty: Some(1.05),
            ..qwen(0.7, 0.8, None)
        }),
        ..BASE
    },
    // Never low effort for agent work (τ-bench 35 at low vs 55 at high).
    Family {
        name: "gpt-oss",
        ids: &["gpt-oss"],
        thinking: Some(tp(1.0, 1.0)),
        effort: Some(ThinkingLevel::Medium),
        replay: Some(ReasoningReplay::Turn),
        edits: EditForm::Patch,
        ..BASE
    },
    Family {
        name: "magistral",
        ids: &["magistral"],
        thinking: Some(tp(0.7, 0.95)),
        mistral_ids: true,
        ..BASE
    },
    Family {
        name: "devstral",
        ids: &["devstral", "mistral-small"],
        instruct: Some(t(0.15)),
        mistral_ids: true,
        ..BASE
    },
    Family {
        name: "mistral",
        ids: &["mistral", "ministral", "mixtral", "codestral", "pixtral"],
        mistral_ids: true,
        ..BASE
    },
    Family {
        name: "glm-4.7",
        ids: &["glm-4.7", "glm4.7"],
        thinking: Some(tp(0.7, 1.0)),
        ..BASE
    },
    Family {
        name: "glm-4.6",
        ids: &["glm-4.6", "glm4.6"],
        thinking: Some(tpk(1.0, 0.95, 40)),
        ..BASE
    },
    Family {
        name: "gemma",
        ids: &["gemma-3", "gemma3", "gemma-4", "gemma4"],
        thinking: Some(tpk(1.0, 0.95, 64)),
        instruct: Some(tpk(1.0, 0.95, 64)),
        ..BASE
    },
    Family {
        name: "llama",
        ids: &["llama-3", "llama3", "llama-4", "llama4"],
        instruct: Some(tp(0.6, 0.9)),
        ..BASE
    },
    Family {
        name: "phi-4-reasoning",
        ids: &["phi-4-reasoning", "phi4-reasoning"],
        thinking: Some(tpk(0.8, 0.95, 50)),
        ..BASE
    },
    Family {
        name: "deepseek-r1",
        ids: &["r1-distill", "deepseek-r1"],
        thinking: Some(tp(0.6, 0.95)),
        edits: EditForm::Replace,
        ..BASE
    },
    Family {
        name: "deepseek",
        ids: &["deepseek"],
        edits: EditForm::Replace,
        ..BASE
    },
    Family {
        name: "minimax",
        ids: &["minimax"],
        thinking: Some(tpk(1.0, 0.95, 40)),
        edits: EditForm::Replace,
        ..BASE
    },
    Family {
        name: "kimi-k2",
        ids: &["kimi-k2"],
        thinking: Some(t(1.0)),
        instruct: Some(t(0.6)),
        edits: EditForm::Replace,
        ..BASE
    },
    // Nemotron 3 Nano's values for tool calling.
    Family {
        name: "nemotron",
        ids: &["nemotron"],
        thinking: Some(tp(0.6, 0.95)),
        ..BASE
    },
    Family {
        name: "smollm3",
        ids: &["smollm3"],
        thinking: Some(tp(0.6, 0.95)),
        instruct: Some(tp(0.6, 0.95)),
        ..BASE
    },
    // GPT and Codex models (hosted; nothing sent but the edit form).
    Family {
        name: "gpt",
        ids: &["gpt-4", "gpt-5", "gpt-6", "codex"],
        edits: EditForm::Patch,
        ..BASE
    },
];

/// Sizes (billions of parameters: total, active) of models whose id says
/// something misleading (`llama-4-scout-17b-16e` is 109B) or a MoE's id
/// that leaves out the active part, looked at before the id's own numbers.
const SIZES_FIRST: &[(&str, (f32, f32))] = &[
    ("gpt-oss-20b", (21.0, 3.6)),
    ("gpt-oss-120b", (117.0, 5.1)),
    ("llama-4-scout", (109.0, 17.0)),
    ("llama-4-maverick", (400.0, 17.0)),
];

/// Sizes of models whose id gives none.
const SIZES_KNOWN: &[(&str, (f32, f32))] = &[
    ("devstral", (24.0, 24.0)),
    ("mistral-small", (24.0, 24.0)),
    ("magistral-small", (24.0, 24.0)),
    ("glm-4.7-flash", (30.0, 3.0)),
    ("glm-4.5-air", (106.0, 12.0)),
    ("glm-4.5", (355.0, 32.0)),
    ("glm-4.6", (355.0, 32.0)),
    ("glm-4.7", (355.0, 32.0)),
    ("minimax", (230.0, 10.0)),
    ("kimi-k2", (1000.0, 32.0)),
    ("deepseek-coder-v2-lite", (16.0, 2.4)),
    ("deepseek", (671.0, 37.0)),
    ("phi-4", (14.0, 14.0)),
];

/// A model's size in billions of parameters (`active` = `total` when dense).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub total: f32,
    pub active: f32,
}

impl Params {
    fn moe(&self) -> bool {
        self.active < self.total * 0.9
    }

    /// Small: dense up to ~35B, or a MoE with up to ~12B active (and not
    /// much more than 120B in all).
    pub fn small(&self) -> bool {
        self.total <= 36.0 || (self.moe() && self.active <= 12.5 && self.total <= 125.0)
    }

    /// Tiny: dense up to ~9B or ~4B active.
    fn tiny(&self) -> bool {
        self.active <= 4.5 || self.total <= 9.5
    }

    /// The context it still works well with: 32k for ~3B-active MoEs and
    /// dense ≤9B (Fiction.LiveBench: Qwen3-30B-A3B 40.6% at 16k), 48k for mid
    /// MoEs; `None` (the window decides) for dense models over ~20B and bigger.
    /// A 32k cap on qwen3.8-27b (262k window) made a 30-module migration in
    /// bench/ thrash: the files it had just read were trimmed, read again and
    /// trimmed again, while the uncapped run passed (batch 14).
    fn effective_context(&self) -> Option<u64> {
        if self.tiny() {
            Some(32_000)
        } else if self.moe() && self.active <= 12.5 && self.total <= 125.0 {
            Some(48_000)
        } else {
            None
        }
    }
}

/// `27b` → 27, `1.5b` → 1.5.
fn billions(t: &str) -> Option<f32> {
    let num = t.strip_suffix('b')?;
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    num.parse::<f32>().ok()
}

/// Size from the id's own parts: `qwen3-30b-a3b` (30B, 3B active),
/// `llama3.1:8b`, `gemma-3n-e4b`, `mixtral-8x7b`.
fn params_in(id: &str) -> Option<Params> {
    let mut total = None;
    let mut active = None;
    for t in id.split(['-', '_', ':', '/', ' ', '@']) {
        if let Some(a) = t.strip_prefix('a').and_then(billions) {
            active.get_or_insert(a);
        } else if let Some(e) = t.strip_prefix('e').and_then(billions) {
            total.get_or_insert(e);
        } else if let Some(n) = billions(t) {
            total.get_or_insert(n);
        } else if let Some((n, m)) = t.split_once('x')
            && let (Ok(n), Some(m)) = (n.parse::<f32>(), billions(m))
        {
            total.get_or_insert(n * m);
            active.get_or_insert(2.0 * m);
        }
    }
    let total = total?;
    Some(Params {
        total,
        active: active.unwrap_or(total).min(total),
    })
}

fn params_of(id: &str) -> Option<Params> {
    let known = |table: &[(&str, (f32, f32))]| {
        table
            .iter()
            .find(|(k, _)| id.contains(k))
            .map(|(_, (total, active))| Params {
                total: *total,
                active: *active,
            })
    };
    known(SIZES_FIRST)
        .or_else(|| params_in(id))
        .or_else(|| known(SIZES_KNOWN))
}

/// What Tessera knows about a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The family row it matched, if any.
    pub family: Option<&'static str>,
    pub params: Option<Params>,
    thinking: Option<Sampling>,
    instruct: Option<Sampling>,
    /// Reasoning effort its maker recommends, where it differs from the
    /// server's or provider's own default.
    pub effort: Option<ThinkingLevel>,
    /// Which past reasoning to send back, where it differs from `all`.
    pub replay: Option<ReasoningReplay>,
    /// Its chat template wants tool call ids of exactly 9 letters and digits.
    pub mistral_ids: bool,
    pub edits: EditForm,
    /// The id names one mode: `-instruct` builds never think, `-thinking`
    /// ones always do.
    only: Option<bool>,
}

/// The profile of `model_id`; `template` is the family its llama.cpp chat
/// template shows (`thinking::template_family`), used when the id matches
/// no family.
pub fn profile(model_id: &str, template: Option<&str>) -> Profile {
    let id = model_id.to_ascii_lowercase();
    let family = FAMILIES
        .iter()
        .find(|f| f.ids.iter().any(|k| id.contains(k)))
        .or_else(|| {
            let t = template?;
            FAMILIES.iter().find(|f| match t {
                "mistral" => f.name == "mistral",
                "gemma" | "llama" | "gpt-oss" => f.name == t,
                _ => false,
            })
        });
    let f = family.unwrap_or(&BASE);
    let only = if id.contains("instruct") {
        Some(false)
    } else if id.contains("thinking") {
        Some(true)
    } else {
        None
    };
    Profile {
        family: family.map(|f| f.name),
        params: params_of(&id),
        thinking: f.thinking,
        instruct: f.instruct,
        effort: f.effort,
        replay: f.replay,
        mistral_ids: f.mistral_ids || template == Some("mistral"),
        edits: f.edits,
        only,
    }
}

impl Profile {
    /// The model card's sampling values with thinking on or off; `None`
    /// when the card is unknown (the server's own defaults stay).
    pub fn sampling(&self, thinking: bool) -> Option<Sampling> {
        let thinking = self.only.unwrap_or(thinking);
        if thinking {
            self.thinking.or(self.instruct)
        } else {
            self.instruct.or(self.thinking)
        }
    }

    /// Known to be small (see [`Params::small`]).
    pub fn small_size(&self) -> bool {
        self.params.is_some_and(|p| p.small())
    }

    /// Known to be bigger than small.
    pub fn large(&self) -> bool {
        self.params.is_some_and(|p| !p.small())
    }

    /// Compact before this many tokens: the context a model of its size
    /// still works well with, at most half its window. `None` for big or
    /// unknown models (the window decides, as before).
    pub fn context_budget(&self, window: u64) -> Option<u64> {
        let fit = self.params?.effective_context()?;
        Some(fit.min(window / 2).max(8_000))
    }
}

/// Whether chats on a model start in small model mode: the user's setting,
/// or for `Auto` a model on the user's own server (unless known to be
/// large), a model known to be small, or one that failed the tool check.
/// Never Claude Code (it runs its own harness).
pub fn small_mode(
    setting: SmallMode,
    kind: Option<ProviderKind>,
    own_server: bool,
    profile: &Profile,
    failed_check: bool,
) -> bool {
    if matches!(kind, None | Some(ProviderKind::ClaudeCode)) {
        return false;
    }
    match setting {
        SmallMode::On => true,
        SmallMode::Off => false,
        SmallMode::Auto => failed_check || profile.small_size() || (own_server && !profile.large()),
    }
}

/// What the engine asks of a request beside the conversation (see
/// `Providers::stream_tuned`).
#[derive(Debug, Clone, Default)]
pub struct Tuning {
    /// Send the model card's sampling values (to the user's own servers).
    pub card_sampling: bool,
    /// The user's own values (`[tuning]`), sent instead where set.
    pub sampling: Sampling,
    /// `parallel_tool_calls`; `None` keeps the default (on).
    pub parallel_calls: Option<bool>,
}

/// Small model mode as a session froze it with its prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Small {
    /// The agent turned the other tools on (one prompt-cache miss).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
    /// Worked examples open the conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub examples: bool,
    /// `edit` takes `old`/`new` only (the profile's edit form).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_only: bool,
}

/// Tools offered in small model mode until the agent turns the others on.
/// Never fewer: hiding core tools or the way to ask the user costs small
/// models more than it saves.
pub const CORE_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "bash",
    "grep",
    "glob",
    "job",
    "task",
    "web_search",
    "web_fetch",
    "memory",
    "docs",
    "whiteboard",
];

/// The only tools a terminal chat (`SessionInfo::lean`) is offered: a lean
/// coding harness. Edits still report a language server's diagnostics.
pub const LEAN_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "bash",
    "job",
    "grep",
    "glob",
    "task",
    "web_search",
    "web_fetch",
];

/// Identical failing calls before the next is refused in small model mode
/// (5 otherwise): Roo, Cline and OpenHands stop loops at 3.
pub const SMALL_REPEAT_LIMIT: u32 = 3;

/// Steps between plan reminders in small model mode.
pub const PLAN_EVERY: u32 = 6;

/// Small model mode: past this many tokens, old tool output is replaced
/// with references in one batch (the newest third stays), long before a
/// big window's compaction. Only where it frees a quarter of it at once, so
/// the prompt cache misses rarely: replayed on bench/ traces of qwen3.8-27b
/// (262k window), 2 of 9 hard tasks trimmed once each, -6.6% input tokens.
pub const OBSERVE_AT: u64 = 48_000;

const EDIT_SMALL: &str = "Change part of a file you have read. Give `path` and either:\n\
- `old`: exact text from the file (it must appear once; `all: true` changes every match) and `new`: what replaces it; or\n\
- `tag` and `at` from your latest read, and `text`: `at` \"12\" replaces line 12, \"12-15\" lines 12 to 15, \">12\" adds after line 12, \">$\" adds at the end; `text` \"\" deletes.\n\
Use `write` for a new file or to rewrite a short one.";

const EDIT_REPLACE_ONLY: &str = "Change part of a file you have read: `path`, `old` (exact text from the file; it must \
appear once, or set `all: true` to change every match) and `new` (what replaces it). Use `write` for a new file or to \
rewrite a short one.";

const TASK_SMALL: &str = "A short plan for work with several steps; it is kept when old messages are summarized. \
`plan`: the steps in order (replaces the plan). `done`: numbers of the steps finished. `goal`: what the user wants. \
No arguments: show it.";

const DOCS_SMALL: &str = "How Tessera works: no arguments lists the topics, `topic` reads one, `query` searches. \
`topic: \"tools\"` turns on the other tools (browser, notes, whiteboards, schedules, subagents, other computers, \
notifications, sharing files, skills, settings) from your next step.";

const ASK_SMALL: &str = "Ask the user a question in a card and wait for the answer: `action: \"ask\"`, `prompt`, and \
`input`: `text`, `choice` (with `options`) or `confirm`. In a chat you can also simply ask in your reply.";

/// The specs a small-mode session is offered: the core tools (all of them
/// once `more` is on), with flat `edit` and `task` arguments.
pub fn small_specs(specs: Vec<ToolSpec>, s: &Small) -> Vec<ToolSpec> {
    specs
        .into_iter()
        .filter_map(|mut t| {
            match t.name.as_str() {
                "edit" => {
                    let lines = json!({"tag": {"type": "string"}, "at": {"type": "string"}, "text": {"type": "string"}});
                    let mut props = json!({
                        "path": {"type": "string"},
                        "old": {"type": "string"},
                        "new": {"type": "string"},
                        "all": {"type": "boolean"}
                    });
                    if !s.replace_only
                        && let (Some(p), Some(l)) = (props.as_object_mut(), lines.as_object())
                    {
                        p.extend(l.clone());
                    }
                    t.description = if s.replace_only {
                        EDIT_REPLACE_ONLY
                    } else {
                        EDIT_SMALL
                    }
                    .into();
                    t.parameters =
                        json!({"type": "object", "properties": props, "required": ["path"]});
                }
                "task" => {
                    t.description = TASK_SMALL.into();
                    t.parameters = json!({
                        "type": "object",
                        "properties": {
                            "goal": {"type": "string"},
                            "plan": {"type": "array", "items": {"type": "string"}},
                            "done": {"type": "array", "items": {"type": "integer"}}
                        }
                    });
                }
                "docs" => t.description = DOCS_SMALL.into(),
                "whiteboard" if !s.more => {
                    t.description = ASK_SMALL.into();
                    t.parameters = json!({
                        "type": "object",
                        "properties": {
                            "action": {"type": "string", "enum": ["ask"]},
                            "prompt": {"type": "string"},
                            "input": {"type": "string", "enum": ["text", "choice", "confirm"]},
                            "options": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["action", "prompt"]
                    });
                }
                name if !s.more && !CORE_TOOLS.contains(&name) => return None,
                _ => {}
            }
            Some(t)
        })
        .collect()
}

// ---- lean start (`[agent] lean_tools`) -------------------------------------
//
// Small model mode's mechanism for every chat: the product tools most chats
// never call wait until first called, listed in one line of the prompt, and
// the largest tools get shorter descriptions (the detail is in `docs`).

/// A lean start as a session froze it with its prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanStart {
    /// The other tools were turned on (one prompt-cache miss).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more: bool,
}

/// The tools a lean start leaves off until one is called, with the few
/// words the prompt's line gives each.
pub const LEAN_START_LATER: &[(&str, &str)] = &[
    ("note", "the user's notes"),
    ("whiteboard", "show visuals, ask in a card"),
    ("browser", "a real browser shared with the user"),
    ("schedule", "scheduled jobs"),
    ("notify", "notifications, reminders"),
    ("share_file", "a file to download"),
    ("save_secret", "keep or list passwords"),
    ("settings", "Tessera's settings"),
    ("machines", "the user's other computers"),
    ("message_bot", "ask another bot"),
    ("agents", "manage subagents"),
    ("skill_manage", "save a skill"),
];

/// Characters of the skills index at a lean start (most used first).
pub const LEAN_START_SKILLS_CHARS: usize = 2_000;

/// Saved passwords listed at a lean start (most recently used first); the
/// rest through `save_secret` `action: "list"`.
pub const LEAN_START_SECRETS: usize = 10;

const EDIT_SHORT: &str = "Edit a file, one mode per call:\n\
1. `lines`: items {at, text} against the version you read, with its `tag`. `at`: \"12\" or \"12-15\" (replace), \
\"<12\"/\">12\" (insert before/after), \">$\" (append); `text` may span lines, \"\" deletes. Numbers are the file as \
read. Only lines a read or grep showed you; an older tag works if those lines didn't change.\n\
2. `replace`: items {old, new}; `old` must match once (or set `all`).";

/// Replaces `tools::meta::AGENT_INTRO` (the agent types after it stay).
const AGENT_SHORT: &str = "Fresh-context subagents for large research or non-overlapping edits. Give self-contained \
prompts and required output; they see none of this chat. Reports arrive here; you resume when all are in. Tell the \
user; don't poll. `wait: true` blocks them too: only if asked. Per task: `agent` (type below), `output` (JSON schema), \
`isolation: \"worktree\"` (private checkout; `docs subagents`).";

const MEMORY_SHORT: &str = "Durable facts for later sessions. `target: user`: the person's preferences, setup, \
conventions, corrections; `target: memory`: your own lessons, where things are, what worked. Short facts, not task \
progress or procedures. `add`; `replace` (`old` → `content`); `remove` (`old`: a short unique part). Space is \
limited: consolidate.";

/// `task`'s schema (the ledger's fields) stays: only the words are fewer.
const TASK_SHORT: &str = "Task. Lists replace; add_* appends; remove archives decisions/artifacts/notes. \
update/verify/remove: 1-based. No args: read.";

/// For when `machines` is on (it starts off).
const MACHINES_SHORT: &str = "The user's other computers (\"machines\"). `list`: those this chat may use, online or \
not, what each allows (folders read-only or read-write, commands) and when it asks first. `run` a command (`cwd` in an \
allowed folder), `read`/`ls`, `write` a file, `edit` (exact `old` → `new`); paths are that machine's own. Commands \
can't add entries to an allowed home folder itself: `write` can. Each machine sets its own limits (a bot may have \
less); nothing here widens them. `docs machines`.";

/// For when `whiteboard` is on (it starts off).
const WHITEBOARD_SHORT: &str = "Visuals the user asked for. `show` a card in the chat: `html` (self-contained, no \
network; `docs whiteboards`), `markdown`, `image_path`, `url` or `local_port` (a web app here); `open: true` also a \
tile, only if asked; `id` updates. `ask` waits for `input`: signature (PNG), text, choice, confirm. `read`/`look` \
(`id`/`title`, else latest): content, or the user's screen of it (also pages they opened); `list`.";

const LIST_SECRETS: &str = " `action: \"list\"`: every saved one this chat may use (names only).";

/// The prompt's line about the tools a lean start left off, for those of
/// them this chat may use (`None`: none).
pub fn lean_start_line(available: &[&str]) -> Option<String> {
    let items: Vec<String> = LEAN_START_LATER
        .iter()
        .filter(|(n, _)| available.contains(n))
        .map(|(n, what)| format!("`{n}` ({what})"))
        .collect();
    (!items.is_empty()).then(|| {
        format!(
            "# More tools\nOff until first called (that call turns them on from your next step, as does `docs` topic \"tools\"): {}.",
            items.join(", ")
        )
    })
}

/// `specs` as a lean start offers them: without [`LEAN_START_LATER`] until
/// `more`, shorter descriptions for the largest tools (`edit`, `agent`,
/// `task`, `memory`, and `machines` and `whiteboard` once on), and the list
/// action on `save_secret`.
pub fn lean_start_specs(specs: Vec<ToolSpec>, l: &LeanStart) -> Vec<ToolSpec> {
    specs
        .into_iter()
        .filter(|t| l.more || !LEAN_START_LATER.iter().any(|(n, _)| *n == t.name))
        .map(|mut t| {
            match t.name.as_str() {
                "edit" => t.description = EDIT_SHORT.into(),
                "memory" => t.description = MEMORY_SHORT.into(),
                "task" => t.description = TASK_SHORT.into(),
                "machines" => t.description = MACHINES_SHORT.into(),
                "whiteboard" => t.description = WHITEBOARD_SHORT.into(),
                "agent" => {
                    if let Some(types) = t.description.strip_prefix(crate::tools::meta::AGENT_INTRO)
                    {
                        t.description = format!("{AGENT_SHORT}{types}");
                    }
                }
                "save_secret" => {
                    t.description.push_str(LIST_SECRETS);
                    if let Some(p) = t.parameters["properties"].as_object_mut() {
                        p.insert(
                            "action".into(),
                            json!({"type": "string", "enum": ["save", "list"]}),
                        );
                    }
                    // `name` isn't needed to list.
                    if let Some(o) = t.parameters.as_object_mut() {
                        o.remove("required");
                    }
                }
                _ => {}
            }
            t
        })
        .collect()
}

/// A flat small-mode call in the shape the tool itself takes:
/// `edit {path, old, new, all}` → `replace: [{old, new, all}]`,
/// `edit {path, tag, at, text}` → `lines: [{at, text}]`,
/// `task {done: [2]}` → `update: [{index: 2, status: "done"}]`. Calls in
/// the full shapes (and every other tool's) come back unchanged.
pub fn expand_flat(tool: &str, mut args: Value) -> Value {
    let Some(obj) = args.as_object_mut() else {
        return args;
    };
    match tool {
        "edit" if !obj.contains_key("replace") && !obj.contains_key("lines") => {
            if obj.contains_key("at") {
                let mut item = serde_json::Map::new();
                for k in ["at", "text"] {
                    if let Some(v) = obj.remove(k) {
                        item.insert(k.into(), v);
                    }
                }
                item.entry("text").or_insert_with(|| json!(""));
                obj.insert("lines".into(), json!([item]));
            } else if obj.contains_key("old") {
                let mut item = serde_json::Map::new();
                for k in ["old", "new", "all"] {
                    if let Some(v) = obj.remove(k) {
                        item.insert(k.into(), v);
                    }
                }
                item.entry("new").or_insert_with(|| json!(""));
                obj.insert("replace".into(), json!([item]));
            }
        }
        "task" => {
            if let Some(done) = obj.remove("done") {
                let steps: Vec<Value> = match done {
                    Value::Array(a) => a,
                    v => vec![v],
                };
                let mut update: Vec<Value> = obj
                    .remove("update")
                    .and_then(|u| u.as_array().cloned())
                    .unwrap_or_default();
                for n in steps {
                    let n = match &n {
                        Value::String(s) => s.trim().parse::<u64>().ok(),
                        v => v.as_u64(),
                    };
                    if let Some(n) = n {
                        update.push(json!({"index": n, "status": "done"}));
                    }
                }
                obj.insert("update".into(), Value::Array(update));
            }
        }
        _ => {}
    }
    args
}

/// What a small-mode `edit` that arrived with nothing to change is told,
/// in the small form's own words and ending with the call that would work
/// (`replace_only`: the form without line edits). A model server drops
/// argument names a tool doesn't list, so the full forms (`lines`,
/// `replace`) arrive as `{path}`; text without `at` or `old` (`{path,
/// text}`) has no place to go.
pub fn empty_edit_hint(args: &Value, replace_only: bool) -> String {
    let path = Value::from(args.get("path").and_then(Value::as_str).unwrap_or("…"));
    let text = ["text", "new", "content"].iter().find_map(|k| {
        args.get(*k)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
    });
    let old_new = format!(
        "{{\"path\": {path}, \"old\": \"<exact text now in the file>\", \"new\": {}}}",
        text.map_or_else(
            || "\"<what replaces it>\"".into(),
            |_| "<your text>".to_string()
        )
    );
    let lines = format!(
        "{{\"path\": {path}, \"tag\": \"<tag from your latest read>\", \"at\": \"12-15\", \"text\": {}}}",
        text.map_or_else(|| "\"<new lines>\"".into(), |_| "<your text>".to_string())
    );
    let start = if text.is_some() {
        "This `edit` has text but nothing that says where it goes."
    } else {
        "This `edit` arrived with nothing to change."
    };
    let whole = if text.is_some() {
        " For a whole new version of the file use `write`."
    } else {
        ""
    };
    if replace_only {
        format!("{start} Send {old_new}.{whole}")
    } else {
        format!(
            "{start} Send {old_new}, or {lines} (`at` \"12\" is line 12, \">$\" adds at the end).{whole}"
        )
    }
}

/// Ids for the worked examples: 9 letters and digits, so every template
/// takes them (Mistral's insists).
const EXAMPLE_IDS: [&str; 5] = [
    "exa1call1",
    "exa1call2",
    "exa1call3",
    "exa1call4",
    "exa1call5",
];

/// The worked examples a small-mode conversation opens with: one short
/// task done the way this harness works (a plan, a read — of two files at
/// once with `several` — an edit, a check), as real messages. Examples as
/// messages teach small models tool use far better than the same text in
/// the prompt.
pub fn examples(several: bool) -> Vec<Msg> {
    let before = "HOST = \"localhost\"\nPORT = 8080\n";
    let after = "HOST = \"localhost\"\nPORT = 9090\n";
    let readme = "Settings are in config.py.\n";
    let (t1, t2, t3) = (
        crate::tools::files::file_tag(before),
        crate::tools::files::file_tag(after),
        crate::tools::files::file_tag(readme),
    );
    let call = |i: usize, name: &str, args: Value| {
        Msg::Assistant(vec![AssistantPart::ToolCall {
            id: EXAMPLE_IDS[i].into(),
            name: name.into(),
            arguments: args,
        }])
    };
    let result = |i: usize, name: &str, text: String| Msg::ToolResult {
        call_id: EXAMPLE_IDS[i].into(),
        name: name.into(),
        content: vec![ContentPart::text(text)],
        is_error: false,
    };
    vec![
        Msg::User(vec![ContentPart::text(
            "[Example of how tools are used here; it is not part of this conversation.]\n\
             In config.py, set the port to 9090 and check it.",
        )]),
        call(
            0,
            "task",
            json!({"plan": ["Change the port in config.py", "Check the change"]}),
        ),
        result(
            0,
            "task",
            "Ledger saved. Plan 0/2 done; next: 1. Change the port in config.py.".into(),
        ),
        if several {
            call(1, "read", json!({"paths": ["config.py", "README.md"]}))
        } else {
            call(1, "read", json!({"path": "config.py"}))
        },
        result(
            1,
            "read",
            if several {
                format!(
                    "[config.py#{t1}] 2 lines\n1:HOST = \"localhost\"\n2:PORT = 8080\n\n\
                     [README.md#{t3}] 1 lines\n1:Settings are in config.py.\n"
                )
            } else {
                format!("[config.py#{t1}] 2 lines\n1:HOST = \"localhost\"\n2:PORT = 8080\n")
            },
        ),
        call(
            2,
            "edit",
            json!({"path": "config.py", "old": "PORT = 8080", "new": "PORT = 9090"}),
        ),
        result(
            2,
            "edit",
            format!(
                "[config.py#{t2}] edited (+1 −1)\n1 HOST = \"localhost\"\n-:PORT = 8080\n2+PORT = 9090\n…"
            ),
        ),
        call(
            3,
            "bash",
            json!({"command": "python3 -c 'import config; print(config.PORT)'"}),
        ),
        result(3, "bash", "9090\n[exit 0 · 0.1s]".into()),
        call(4, "task", json!({"done": [1, 2]})),
        result(4, "task", "Ledger saved. Plan 2/2 done.".into()),
        Msg::Assistant(vec![AssistantPart::Text {
            text: "The port in config.py is now 9090, and Python reads it back as 9090.".into(),
        }]),
    ]
}

/// The tools the examples call (all must be offered for them to be shown).
pub const EXAMPLE_TOOLS: &[&str] = &["task", "read", "edit", "bash"];

/// "[Plan: step 2/5 — Run the tests]" while the ledger has open steps.
pub fn plan_line(l: &TaskLedger) -> Option<String> {
    let (i, next) = l
        .plan
        .iter()
        .enumerate()
        .find(|(_, p)| matches!(p.status, PlanStatus::Pending | PlanStatus::InProgress))?;
    Some(format!(
        "[Plan: step {}/{} — {}]",
        i + 1,
        l.plan.len(),
        crate::util::one_line(&next.text, 120)
    ))
}

/// A tool call id as Mistral's templates want it: exactly 9 letters and
/// digits. Ids already like that stay; others get a stable stand-in made
/// from them (the same every request, so prompt caches hold).
pub fn mistral_id(id: &str) -> String {
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return id.to_string();
    }
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut h: u64 = 0xcbf29ce484222325;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (0..9)
        .map(|_| {
            let c = ALPHABET[(h % 62) as usize] as char;
            h /= 62;
            if h == 0 {
                h = 0x9e3779b97f4a7c15;
            }
            c
        })
        .collect()
}

/// The one-call tool check: a model on the user's own server is asked once
/// to use a single tool, so a model that can't is noticed before it fails
/// at a task (and runs in small model mode).
pub mod check {
    use super::*;

    pub const TOOL: &str = "get_time";
    pub const SYSTEM: &str = "You are a helpful assistant. Use a tool when it helps.";
    pub const PROMPT: &str = "What time is it? Use the tool.";

    pub fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: TOOL.into(),
            description: "The current date and time.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }]
    }

    /// What a passed check looks like: a call to the tool (made, or written
    /// as text and recovered) with arguments that parse.
    pub fn passed(content: &[AssistantPart]) -> bool {
        content.iter().any(|p| {
            matches!(p, AssistantPart::ToolCall { name, arguments, .. }
                if name == TOOL && arguments.get("__invalid_arguments").is_none())
        })
    }

    /// What the chat says when a model failed it.
    pub fn failed_notice(model: &str) -> String {
        format!(
            "{model} didn't use a tool when asked to in a quick test, so it may not manage tasks that need tools \
             (reading files, running commands). It can still chat. Small model mode is now on for it: simpler \
             tools and shorter instructions. Models known to use tools well, such as recent Qwen, Gemma or gpt-oss \
             models, may do better."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> Profile {
        profile(id, None)
    }

    #[test]
    fn qwen_families_follow_their_cards() {
        let q = p("qwen3.8-27b");
        assert_eq!(q.family, Some("qwen3.6"));
        let think = q.sampling(true).unwrap();
        assert_eq!(think.temperature, Some(1.0));
        assert_eq!(think.top_p, Some(0.95));
        assert_eq!(think.top_k, Some(20));
        assert_eq!(think.min_p, Some(0.0));
        assert_eq!(think.presence_penalty, Some(0.0));
        let plain = q.sampling(false).unwrap();
        assert_eq!(
            (plain.temperature, plain.top_p, plain.presence_penalty),
            (Some(0.7), Some(0.8), Some(1.5))
        );
        // Qwen3.8 keeps its own effort default (xhigh).
        assert_eq!(q.effort, None);
        assert_eq!(q.replay, None);
        assert_eq!(p("qwen3.8-27b-uncensored").family, Some("qwen3.6"));
        assert_eq!(p("Qwen3.6-35B-A3B").family, Some("qwen3.6"));
        let q35 = p("qwen3.5-9b").sampling(true).unwrap();
        assert_eq!(q35.temperature, Some(0.6));
        let q3 = p("Qwen3-30B-A3B");
        assert_eq!(q3.family, Some("qwen3"));
        assert_eq!(q3.sampling(true).unwrap().temperature, Some(0.6));
        assert_eq!(q3.sampling(true).unwrap().presence_penalty, None);
        assert_eq!(q3.replay, Some(ReasoningReplay::Turn));
        // An `-instruct` build never thinks; a `-thinking` one always does.
        let inst = p("Qwen3-30B-A3B-Instruct-2507");
        assert_eq!(inst.sampling(true).unwrap().temperature, Some(0.7));
        let thinks = p("Qwen3-30B-A3B-Thinking-2507");
        assert_eq!(thinks.sampling(false).unwrap().temperature, Some(0.6));
        let coder = p("Qwen3-Coder-30B-A3B-Instruct").sampling(true).unwrap();
        assert_eq!(coder.repetition_penalty, Some(1.05));
        assert_eq!(coder.temperature, Some(0.7));
        assert_eq!(p("qwq-32b").family, Some("qwen3"));
        assert_eq!(
            p("qwen2.5-coder:32b-instruct-q4_K_M")
                .sampling(false)
                .unwrap()
                .repetition_penalty,
            Some(1.05)
        );
    }

    #[test]
    fn other_families_follow_their_cards() {
        let oss = p("gpt-oss-20b");
        assert_eq!(oss.sampling(true), Some(tp(1.0, 1.0)));
        assert_eq!(oss.effort, Some(ThinkingLevel::Medium));
        assert_eq!(oss.edits, EditForm::Patch);
        let dev = p("devstral-small-2507");
        assert_eq!(dev.sampling(false).unwrap().temperature, Some(0.15));
        assert!(dev.mistral_ids);
        assert!(p("mistral-small-3.2-24b-instruct-2506").mistral_ids);
        assert!(p("Ministral-8B-Instruct").mistral_ids);
        assert_eq!(
            p("Magistral-Small-2509").sampling(true),
            Some(tp(0.7, 0.95))
        );
        assert_eq!(p("GLM-4.7-Flash").sampling(true), Some(tp(0.7, 1.0)));
        assert_eq!(
            p("gemma-4-31b-it").sampling(false),
            Some(tpk(1.0, 0.95, 64))
        );
        assert_eq!(p("llama3.1:8b").sampling(false), Some(tp(0.6, 0.9)));
        assert_eq!(
            p("DeepSeek-R1-Distill-Qwen-32B").sampling(true),
            Some(tp(0.6, 0.95))
        );
        assert_eq!(p("MiniMax-M2").edits, EditForm::Replace);
        assert_eq!(
            p("phi-4-reasoning").sampling(true),
            Some(tpk(0.8, 0.95, 50))
        );
        // Never temperature 0 for a model that thinks.
        for f in FAMILIES {
            for s in [f.thinking].into_iter().flatten() {
                assert!(s.temperature.is_none_or(|t| t > 0.0), "{}", f.name);
            }
        }
        // Unknown models get nothing: the server's own defaults stay.
        let unknown = p("my-finetune");
        assert_eq!(unknown.family, None);
        assert_eq!(unknown.sampling(true), None);
        assert!(!unknown.mistral_ids);
        // Claude and GPT: nothing to send, edit forms only.
        assert_eq!(p("claude-opus-5-5").sampling(true), None);
        assert_eq!(p("gpt-5.5").edits, EditForm::Patch);
    }

    #[test]
    fn a_template_names_the_family_an_id_doesnt() {
        let m = profile("model.gguf", Some("mistral"));
        assert!(m.mistral_ids);
        assert_eq!(m.family, Some("mistral"));
        assert_eq!(
            profile("local-model", Some("gemma")).sampling(true),
            Some(tpk(1.0, 0.95, 64))
        );
        assert_eq!(
            profile("local-model", Some("gpt-oss")).family,
            Some("gpt-oss")
        );
        // The id wins over the template.
        assert_eq!(
            profile("qwen3.8-27b", Some("llama")).family,
            Some("qwen3.6")
        );
        assert_eq!(profile("x", Some("chatml")).family, None);
    }

    #[test]
    fn sizes_from_ids_and_known_models() {
        let size = |id: &str| params_of(&id.to_ascii_lowercase()).map(|p| (p.total, p.active));
        assert_eq!(size("qwen3.8-27b"), Some((27.0, 27.0)));
        assert_eq!(size("Qwen3-30B-A3B-Instruct-2507"), Some((30.0, 3.0)));
        assert_eq!(size("qwen3-235b-a22b"), Some((235.0, 22.0)));
        assert_eq!(size("llama3.1:8b"), Some((8.0, 8.0)));
        assert_eq!(
            size("qwen2.5-coder:32b-instruct-q4_K_M"),
            Some((32.0, 32.0))
        );
        assert_eq!(size("gemma-3n-e4b-it"), Some((4.0, 4.0)));
        assert_eq!(size("qwen2.5-1.5b"), Some((1.5, 1.5)));
        assert_eq!(size("gemini-1.5-pro-1m"), None);
        assert_eq!(size("mixtral-8x7b"), Some((56.0, 14.0)));
        assert_eq!(size("gpt-oss-20b"), Some((21.0, 3.6)));
        assert_eq!(size("llama-4-scout-17b-16e-instruct"), Some((109.0, 17.0)));
        assert_eq!(size("devstral-small-2507"), Some((24.0, 24.0)));
        assert_eq!(size("devstral-2-123b-instruct-2512"), Some((123.0, 123.0)));
        assert_eq!(size("deepseek-r1-distill-qwen-7b"), Some((7.0, 7.0)));
        assert_eq!(size("deepseek-v3.2"), Some((671.0, 37.0)));
        assert_eq!(size("claude-opus-5-5"), None);
        assert_eq!(size("gpt-5.5"), None);
        assert_eq!(size("qwen3.8"), None);
        // Small: dense ≤ ~35B, MoE ≤ ~12B active.
        assert!(p("qwen3.8-27b").small_size());
        assert!(p("gpt-oss-120b").small_size());
        assert!(p("glm-4.5-air").small_size());
        assert!(!p("qwen3-235b-a22b").small_size() && p("qwen3-235b-a22b").large());
        assert!(!p("llama-3.3-70b-instruct").small_size());
        assert!(!p("MiniMax-M2").small_size());
        assert!(!p("claude-opus-5-5").small_size() && !p("claude-opus-5-5").large());
    }

    #[test]
    fn context_budgets_by_size() {
        // Dense over ~20B: the window decides (a 32k cap made bench/ thrash).
        assert_eq!(p("qwen3.8-27b").context_budget(262_144), None);
        // ~3B-active MoE and small dense: 32k, at most half the window; mid MoE: 48k.
        assert_eq!(p("qwen3-8b").context_budget(40_000), Some(20_000));
        assert_eq!(p("Qwen3-30B-A3B").context_budget(131_072), Some(32_000));
        assert_eq!(p("gpt-oss-20b").context_budget(131_072), Some(32_000));
        assert_eq!(p("qwen3-8b").context_budget(131_072), Some(32_000));
        assert_eq!(p("gpt-oss-120b").context_budget(131_072), Some(48_000));
        // Frontier and unknown: unchanged.
        assert_eq!(p("claude-opus-5-5").context_budget(1_000_000), None);
        assert_eq!(p("qwen3-235b-a22b").context_budget(131_072), None);
        assert_eq!(p("m").context_budget(100_000), None);
    }

    #[test]
    fn the_budget_caps_the_compaction_trigger() {
        let mut cfg = crate::config::ContextConfig::default();
        let open = crate::context::budget(262_144, 32_768, &cfg);
        assert!(open.trigger > 150_000, "{}", open.trigger);
        cfg.budget = p("qwen3-8b").context_budget(262_144).unwrap();
        let capped = crate::context::budget(262_144, 32_768, &cfg);
        assert_eq!(capped.trigger, 32_000);
        assert!(capped.keep_recent <= capped.trigger / 3);
        assert_eq!(capped.window, open.window, "the window itself stays");
    }

    #[test]
    fn small_mode_auto_on_and_off() {
        use ProviderKind::*;
        let local = |id: &str| small_mode(SmallMode::Auto, Some(OpenaiChat), true, &p(id), false);
        let hosted = |kind, id: &str| small_mode(SmallMode::Auto, Some(kind), false, &p(id), false);
        // Auto: the user's own server (unless known large), known small models.
        assert!(local("qwen3.8-27b"));
        assert!(local("my-finetune"));
        assert!(!local("llama-3.3-70b-instruct"));
        assert!(hosted(OpenaiChat, "qwen/qwen3-8b"));
        assert!(!hosted(OpenaiCodex, "gpt-5.5"));
        assert!(!hosted(Anthropic, "claude-opus-5-5"));
        assert!(!hosted(OpenaiChat, "deepseek-v3.2"));
        // A failed tool check turns Auto on; the user's choice wins.
        assert!(small_mode(
            SmallMode::Auto,
            Some(OpenaiChat),
            false,
            &p("x"),
            true
        ));
        assert!(small_mode(
            SmallMode::On,
            Some(Anthropic),
            false,
            &p("claude-opus-5-5"),
            false
        ));
        assert!(!small_mode(
            SmallMode::Off,
            Some(OpenaiChat),
            true,
            &p("qwen3.8-27b"),
            true
        ));
        // Claude Code runs its own harness: never.
        assert!(!small_mode(
            SmallMode::On,
            Some(ClaudeCode),
            false,
            &p("haiku"),
            false
        ));
        assert!(!small_mode(SmallMode::On, None, false, &p("x"), false));
    }

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} in full"),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn small_mode_offers_the_core_tools_until_the_rest_are_turned_on() {
        let all = [
            "read",
            "write",
            "edit",
            "grep",
            "glob",
            "bash",
            "job",
            "web_fetch",
            "web_search",
            "task",
            "memory",
            "skill",
            "skill_manage",
            "recall",
            "agent",
            "agents",
            "schedule",
            "message_bot",
            "whiteboard",
            "browser",
            "note",
            "notify",
            "share_file",
            "docs",
            "save_secret",
            "settings",
            "machines",
            "lsp",
            "mcp__x__y",
        ];
        let specs: Vec<ToolSpec> = all.iter().copied().map(spec).collect();
        let names = |s: &[ToolSpec]| s.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let small = small_specs(specs.clone(), &Small::default());
        assert_eq!(
            names(&small),
            [
                "read",
                "write",
                "edit",
                "grep",
                "glob",
                "bash",
                "job",
                "web_fetch",
                "web_search",
                "task",
                "memory",
                "whiteboard",
                "docs"
            ]
        );
        let edit = small.iter().find(|t| t.name == "edit").unwrap();
        assert!(edit.parameters["properties"]["old"].is_object());
        assert!(edit.parameters["properties"]["at"].is_object());
        assert!(edit.parameters["properties"].get("replace").is_none());
        let board = small.iter().find(|t| t.name == "whiteboard").unwrap();
        assert_eq!(
            board.parameters["properties"]["action"]["enum"],
            json!(["ask"])
        );
        // Exact replacements only, for models that fumble line numbers.
        let replace = small_specs(
            specs.clone(),
            &Small {
                replace_only: true,
                ..Default::default()
            },
        );
        let edit = replace.iter().find(|t| t.name == "edit").unwrap();
        assert!(edit.parameters["properties"].get("at").is_none());
        // Everything once turned on, with the flat forms kept.
        let more = small_specs(
            specs,
            &Small {
                more: true,
                ..Default::default()
            },
        );
        assert_eq!(more.len(), all.len());
        assert_eq!(
            more.iter()
                .find(|t| t.name == "whiteboard")
                .unwrap()
                .description,
            "whiteboard in full"
        );
        assert!(
            more.iter().find(|t| t.name == "edit").unwrap().parameters["properties"]["old"]
                .is_object()
        );
    }

    #[test]
    fn a_lean_start_keeps_the_product_tools_for_later() {
        let all = [
            "read",
            "edit",
            "memory",
            "agent",
            "note",
            "browser",
            "save_secret",
            "machines",
            "lsp",
            "mcp__x__y",
        ];
        let mut specs: Vec<ToolSpec> = all.iter().copied().map(spec).collect();
        specs[3].description = format!(
            "{}\nTypes: explore (search).",
            crate::tools::meta::AGENT_INTRO
        );
        specs[6].parameters = json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]});
        let names = |s: &[ToolSpec]| s.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let lean = lean_start_specs(specs.clone(), &LeanStart::default());
        // The user's own MCP tools and a language server stay.
        assert_eq!(
            names(&lean),
            ["read", "edit", "memory", "agent", "lsp", "mcp__x__y"]
        );
        let agent = &lean[3].description;
        assert!(agent.starts_with(AGENT_SHORT) && agent.ends_with("\nTypes: explore (search)."));
        assert_eq!(lean[1].description, EDIT_SHORT);
        let more = lean_start_specs(specs, &LeanStart { more: true });
        assert_eq!(more.len(), all.len());
        let secret = more.iter().find(|t| t.name == "save_secret").unwrap();
        assert!(secret.parameters["properties"]["action"].is_object());
        assert!(secret.parameters.get("required").is_none());
        let machines = more.iter().find(|t| t.name == "machines").unwrap();
        assert_eq!(machines.description, MACHINES_SHORT);
        // Each shorter description is shorter than the tool's own, and the
        // rules that keep it safe or working are still in it.
        use crate::tools::Tool;
        for (short, own, keeps) in [
            (
                EDIT_SHORT,
                crate::tools::files::Edit.description(),
                "an older tag works",
            ),
            (
                MEMORY_SHORT,
                crate::tools::meta::MemoryTool.description(),
                "not task",
            ),
            (
                TASK_SHORT,
                crate::tools::meta::Task.description(),
                "1-based",
            ),
            (
                MACHINES_SHORT,
                crate::tools::machines::Machines.description(),
                "nothing here widens them",
            ),
            (
                WHITEBOARD_SHORT,
                crate::tools::board::Whiteboard.description(),
                "only if asked",
            ),
            (
                AGENT_SHORT,
                crate::tools::meta::AGENT_INTRO.into(),
                "self-contained",
            ),
        ] {
            assert!(short.len() < own.len(), "{short}");
            assert!(short.contains(keeps), "{short}");
        }
        // The line names only what the chat may use.
        let line = lean_start_line(&["read", "note", "machines"]).unwrap();
        assert!(line.contains("`note` (") && line.contains("`machines` ("));
        assert!(!line.contains("browser"));
        assert_eq!(lean_start_line(&["read"]), None);
    }

    #[test]
    fn flat_calls_take_the_tools_own_shape() {
        assert_eq!(
            expand_flat(
                "edit",
                json!({"path": "a", "old": "x", "new": "y", "all": true})
            ),
            json!({"path": "a", "replace": [{"old": "x", "new": "y", "all": true}]})
        );
        assert_eq!(
            expand_flat(
                "edit",
                json!({"path": "a", "tag": "AB12", "at": "3", "text": "z"})
            ),
            json!({"path": "a", "tag": "AB12", "lines": [{"at": "3", "text": "z"}]})
        );
        // Deleting: no `new` / `text` means empty.
        assert_eq!(
            expand_flat("edit", json!({"path": "a", "old": "x"})),
            json!({"path": "a", "replace": [{"old": "x", "new": ""}]})
        );
        assert_eq!(
            expand_flat("task", json!({"done": [1, "2"]})),
            json!({"update": [{"index": 1, "status": "done"}, {"index": 2, "status": "done"}]})
        );
        assert_eq!(
            expand_flat("task", json!({"done": 3})),
            json!({"update": [{"index": 3, "status": "done"}]})
        );
        // Full shapes and other tools stay as they are.
        let full = json!({"path": "a", "replace": [{"old": "x", "new": "y"}]});
        assert_eq!(expand_flat("edit", full.clone()), full);
        let plan = json!({"plan": ["a", "b"]});
        assert_eq!(expand_flat("task", plan.clone()), plan);
        let other = json!({"old": "x", "new": "y"});
        assert_eq!(expand_flat("write", other.clone()), other);
    }

    #[test]
    fn an_empty_small_edit_is_told_the_call_that_would_work() {
        let bare = empty_edit_hint(&json!({"path": "a.py"}), false);
        assert_eq!(
            bare,
            "This `edit` arrived with nothing to change. Send {\"path\": \"a.py\", \"old\": \"<exact text now in the file>\", \"new\": \"<what replaces it>\"}, or {\"path\": \"a.py\", \"tag\": \"<tag from your latest read>\", \"at\": \"12-15\", \"text\": \"<new lines>\"} (`at` \"12\" is line 12, \">$\" adds at the end)."
        );
        // Never the full form's names, which the server would drop again.
        for args in [
            json!({"path": "a.py"}),
            json!({"path": "a.py", "text": "x = 2"}),
        ] {
            for replace_only in [false, true] {
                let m = empty_edit_hint(&args, replace_only);
                assert!(!m.contains("`lines`") && !m.contains("`replace`"), "{m}");
                assert_eq!(m.contains("\"at\""), !replace_only, "{m}");
            }
        }
        let text = empty_edit_hint(&json!({"path": "a.py", "text": "x = 2"}), true);
        assert_eq!(
            text,
            "This `edit` has text but nothing that says where it goes. Send {\"path\": \"a.py\", \"old\": \"<exact text now in the file>\", \"new\": <your text>}. For a whole new version of the file use `write`."
        );
    }

    #[test]
    fn examples_are_stable_and_well_formed() {
        let a = examples(true);
        assert_eq!(format!("{a:?}"), format!("{:?}", examples(true)));
        assert_eq!(examples(false).len(), a.len());
        assert!(matches!(&a[0], Msg::User(_)));
        assert!(
            matches!(a.last(), Some(Msg::Assistant(p)) if matches!(p[0], AssistantPart::Text { .. }))
        );
        let mut called = Vec::new();
        for m in &a {
            match m {
                Msg::Assistant(parts) => {
                    for part in parts {
                        if let AssistantPart::ToolCall { id, name, .. } = part {
                            assert_eq!(mistral_id(id), *id, "ids fit every template");
                            called.push((id.clone(), name.clone()));
                        }
                    }
                }
                Msg::ToolResult { call_id, name, .. } => {
                    let (id, n) = called.last().unwrap();
                    assert_eq!((call_id, name), (id, n));
                }
                Msg::User(_) => {}
            }
        }
        for t in EXAMPLE_TOOLS {
            assert!(called.iter().any(|(_, n)| n == t), "{t}");
        }
        // The flat calls in them work as the tools take them.
        assert!(
            expand_flat(
                "edit",
                json!({"path": "config.py", "old": "PORT = 8080", "new": "PORT = 9090"})
            )
            .get("replace")
            .is_some()
        );
    }

    #[test]
    fn plan_lines_name_the_next_step() {
        use tessera_proto::PlanItem;
        let mut l = TaskLedger::default();
        assert_eq!(plan_line(&l), None);
        l.plan = vec![
            PlanItem {
                text: "Read the code".into(),
                status: PlanStatus::Done,
            },
            PlanItem {
                text: "Run the tests".into(),
                status: PlanStatus::Pending,
            },
            PlanItem {
                text: "Report".into(),
                status: PlanStatus::Pending,
            },
        ];
        assert_eq!(
            plan_line(&l).as_deref(),
            Some("[Plan: step 2/3 — Run the tests]")
        );
        for p in &mut l.plan {
            p.status = PlanStatus::Done;
        }
        assert_eq!(plan_line(&l), None);
    }

    #[test]
    fn mistral_ids_are_nine_letters_and_digits_and_stable() {
        for id in ["call_0_abc", "call_x7Kp2Lm9Q", "toolu_01ABC", "", "c1"] {
            let m = mistral_id(id);
            assert_eq!(m.len(), 9, "{id}");
            assert!(m.bytes().all(|b| b.is_ascii_alphanumeric()), "{m}");
            assert_eq!(m, mistral_id(id));
        }
        assert_eq!(mistral_id("Ab3dE6gH9"), "Ab3dE6gH9");
        assert_ne!(mistral_id("call_1"), mistral_id("call_2"));
    }

    #[test]
    fn the_check_passes_on_a_call_and_fails_on_text() {
        let call = |args: Value| AssistantPart::ToolCall {
            id: "c".into(),
            name: check::TOOL.into(),
            arguments: args,
        };
        assert!(check::passed(&[call(json!({}))]));
        assert!(!check::passed(&[call(json!({"__invalid_arguments": "{"}))]));
        assert!(!check::passed(&[AssistantPart::Text {
            text: "It is noon.".into()
        }]));
        assert!(!check::passed(&[AssistantPart::ToolCall {
            id: "c".into(),
            name: "bash".into(),
            arguments: json!({}),
        }]));
    }
}
