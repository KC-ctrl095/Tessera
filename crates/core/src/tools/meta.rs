//! Harness tools: task ledger, memory, skills, history recall, subagents,
//! scheduled jobs.

use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{
    BotHandoff, CriterionState, CriterionStatus, EntryBody, Job, MemoryTarget, PermissionMode,
    PlanItem, PlanStatus, TaskLedger,
};

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool, opt_str, opt_u64};
use crate::memory::MemoryOp;

fn str_list(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(s) if !s.trim().is_empty() => Some(vec![s.trim().to_string()]),
        _ => None,
    }
}

fn parse_status(s: &str) -> PlanStatus {
    match s
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
        .as_str()
    {
        "in_progress" | "active" | "doing" | "started" => PlanStatus::InProgress,
        "done" | "completed" | "complete" | "finished" => PlanStatus::Done,
        "blocked" => PlanStatus::Blocked,
        "skipped" | "cancelled" | "canceled" => PlanStatus::Skipped,
        _ => PlanStatus::Pending,
    }
}

/// Plain-text rendering used in tool results and compaction checkpoints.
pub fn render_ledger(l: &TaskLedger) -> String {
    let mut out = String::new();
    if !l.goal.is_empty() {
        out.push_str(&format!("Goal: {}\n", l.goal));
    }
    let section = |out: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            out.push_str(&format!("{title}:\n"));
            for (index, i) in items.iter().enumerate() {
                out.push_str(&format!("{}. {i}\n", index + 1));
            }
        }
    };
    section(&mut out, "Constraints", &l.constraints);
    if !l.plan.is_empty() {
        out.push_str("Plan:\n");
        for (i, p) in l.plan.iter().enumerate() {
            let mark = match p.status {
                PlanStatus::Done => "[x]",
                PlanStatus::InProgress => "[>]",
                PlanStatus::Blocked => "[!]",
                PlanStatus::Skipped => "[-]",
                PlanStatus::Pending => "[ ]",
            };
            out.push_str(&format!("{}. {mark} {}\n", i + 1, p.text));
        }
    }
    section(&mut out, "Decisions", &l.decisions);
    section(&mut out, "Open questions", &l.open_questions);
    section(&mut out, "Artifacts", &l.artifacts);
    section(&mut out, "Notes", &l.notes);
    if !l.done_criteria.is_empty() {
        out.push_str("Done when:\n");
        for (i, c) in l.done_criteria.iter().enumerate() {
            let st = l.criteria.get(i);
            let mark = match st.map(|s| s.status).unwrap_or_default() {
                CriterionStatus::Unchecked => "[ ]",
                CriterionStatus::Met => "[met]",
                CriterionStatus::Unmet => "[unmet]",
                CriterionStatus::Unverifiable => "[?]",
            };
            out.push_str(&format!("{}. {mark} {c}", i + 1));
            if let Some(e) = st.map(|s| s.evidence.trim()).filter(|e| !e.is_empty()) {
                out.push_str(&format!(" — {}", crate::util::one_line(e, 120)));
            }
            out.push('\n');
        }
    }
    for (section, archive) in &l.archived {
        out.push_str(&format!(
            "{section}: {} older items archived; use recall artifact={}\n",
            archive.count, archive.artifact
        ));
    }
    if out.is_empty() {
        "(empty)".into()
    } else {
        out
    }
}
/// Only explicitly blocked plan items are waiting. The ledger has no
/// per-item helper dependency links, so unrelated helper activity cannot
/// establish that an in-progress item or an open criterion is blocked.
pub fn completion_note(l: &TaskLedger) -> Option<String> {
    let open: Vec<String> = l
        .done_criteria
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            matches!(
                l.criteria.get(*i).map(|c| c.status).unwrap_or_default(),
                CriterionStatus::Unchecked | CriterionStatus::Unmet
            )
        })
        .map(|(i, c)| {
            format!(
                "{}. {c} [{}]",
                i + 1,
                if l.criteria
                    .get(i)
                    .is_some_and(|c| c.status == CriterionStatus::Unmet)
                {
                    "unmet"
                } else {
                    "unchecked"
                }
            )
        })
        .collect();
    let plan: Vec<String> = l
        .plan
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.status, PlanStatus::Pending | PlanStatus::InProgress))
        .map(|(i, p)| {
            let st = if p.status == PlanStatus::InProgress {
                "in progress"
            } else {
                "pending"
            };
            format!("{}. {} [{st}]", i + 1, p.text)
        })
        .collect();
    if open.is_empty() && plan.is_empty() {
        return None;
    }
    // The user already read the answer: the reply to this is a bare mark
    // (hidden in the chat) unless there is real work left to do.
    let mut s = String::from("[Completion check] Still open in the task ledger:");
    if !open.is_empty() {
        s.push_str("\nDone when:\n");
        s.push_str(&open.join("\n"));
    }
    if !plan.is_empty() {
        s.push_str("\nPlan:\n");
        s.push_str(&plan.join("\n"));
    }
    s.push_str(&format!(
        "\nIf these are done, record it with `task` (`verify` with evidence, or `update`). If one is unmet and you can fix it now, \
         keep working. Otherwise (done, or left open on purpose) reply with only {CHECK_ACK} — your answer above stands."
    ));
    Some(s)
}

/// Digest just the open items and their statuses: unrelated notes, evidence,
/// numbering and closed work do not trigger another model turn.
pub fn completion_state(l: &TaskLedger) -> String {
    use sha2::{Digest, Sha256};
    let mut open: Vec<String> = l
        .plan
        .iter()
        .filter(|p| !matches!(p.status, PlanStatus::Done | PlanStatus::Skipped))
        .map(|p| format!("plan:{:?}:{}", p.status, p.text))
        .collect();
    open.extend(l.done_criteria.iter().enumerate().filter_map(|(i, text)| {
        let status = l
            .criteria
            .get(i)
            .map_or(CriterionStatus::Unchecked, |c| c.status);
        matches!(status, CriterionStatus::Unchecked | CriterionStatus::Unmet)
            .then(|| format!("criterion:{status:?}:{text}"))
    }));
    open.sort();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&open).expect("strings serialize"))
    )
}
/// The agent's whole reply to a completion check when nothing is left: the
/// chat hides it, and reports and previews look past it to the real answer.
pub const CHECK_ACK: &str = "✓";

pub use tessera_proto::{
    CHECK_AUTHOR, check_ack_parts, is_check_ack, is_protocol_body, is_protocol_entry,
};

/// Hold a short acknowledgement-shaped prefix until it becomes real content.
/// This also handles unsolicited/legacy marks without leaking them live.
pub fn possible_check_ack(text: &str) -> bool {
    let t = text
        .trim()
        .trim_matches(|c: char| matches!(c, '✓' | '✔' | '✅' | '.' | '!' | ' '))
        .to_ascii_lowercase();
    !t.contains('\n')
        && ["done", "checked", "ok", "okay", "acknowledged"]
            .iter()
            .any(|w| w.starts_with(&t))
}

/// Arguments that change the ledger (anything else just reads it).
const LEDGER_FIELDS: [&str; 14] = [
    "goal",
    "constraints",
    "plan",
    "update",
    "add_decisions",
    "open_questions",
    "add_artifacts",
    "add_notes",
    "decisions",
    "artifacts",
    "notes",
    "remove",
    "done_criteria",
    "verify",
];

/// Size limits are per section, not per item. Whole older items move to
/// session-owned recall artifacts; never squeeze or silently truncate them.
const LEDGER_SECTION_BYTES: usize = 2_048;
type ArchiveItems = std::collections::BTreeMap<String, Vec<String>>;

fn archive_list(name: &str, list: &mut Vec<String>, archived: &mut ArchiveItems) {
    while list.iter().map(|s| s.len() + 4).sum::<usize>() > LEDGER_SECTION_BYTES {
        archived
            .entry(name.into())
            .or_default()
            .push(list.remove(0));
    }
}

fn bound_ledger(l: &mut TaskLedger, archived: &mut ArchiveItems) {
    for (name, list) in [
        ("decisions", &mut l.decisions),
        ("artifacts", &mut l.artifacts),
        ("notes", &mut l.notes),
    ] {
        archive_list(name, list, archived);
    }
    // Never discard active requirements or let size pruning silently mark a
    // job done. Only settled plan/criteria entries are eligible for archival.
    while l.plan.iter().map(|p| p.text.len() + 16).sum::<usize>() > LEDGER_SECTION_BYTES {
        let Some(i) = l
            .plan
            .iter()
            .position(|p| matches!(p.status, PlanStatus::Done | PlanStatus::Skipped))
        else {
            break;
        };
        let item = l.plan.remove(i);
        archived
            .entry("plan".into())
            .or_default()
            .push(serde_json::to_string(&item).expect("plan serializes"));
    }
    while criteria_bytes(l) > LEDGER_SECTION_BYTES {
        let Some(i) = l.criteria.iter().take(l.done_criteria.len()).position(|c| {
            matches!(
                c.status,
                CriterionStatus::Met | CriterionStatus::Unverifiable
            )
        }) else {
            break;
        };
        let text = l.done_criteria.remove(i);
        let state = l.criteria.remove(i);
        archived
            .entry("done_criteria".into())
            .or_default()
            .push(json!({"text": text, "state": state}).to_string());
    }
}

fn criteria_bytes(l: &TaskLedger) -> usize {
    l.done_criteria
        .iter()
        .enumerate()
        .map(|(i, c)| c.len() + l.criteria.get(i).map_or(0, |s| s.evidence.len()) + 16)
        .sum()
}

fn validate_live_ledger(l: &TaskLedger) -> anyhow::Result<()> {
    for (name, bytes) in [
        ("goal", l.goal.len()),
        (
            "constraints",
            l.constraints.iter().map(|s| s.len() + 4).sum(),
        ),
        (
            "open_questions",
            l.open_questions.iter().map(|s| s.len() + 4).sum(),
        ),
        ("plan", l.plan.iter().map(|p| p.text.len() + 16).sum()),
        ("done_criteria", criteria_bytes(l)),
    ] {
        anyhow::ensure!(
            bytes <= LEDGER_SECTION_BYTES,
            "{name} exceeds the {LEDGER_SECTION_BYTES}-byte live section budget. No update saved: active requirements were not archived or truncated. Keep current state here and detailed evidence in notes/artifacts (retrievable with recall)."
        );
    }
    Ok(())
}

fn replace_list(name: &str, list: &mut Vec<String>, new: Vec<String>, archived: &mut ArchiveItems) {
    for item in list.iter().filter(|item| !new.contains(item)) {
        archived.entry(name.into()).or_default().push(item.clone());
    }
    *list = new;
}

fn save_archives(
    l: &mut TaskLedger,
    items: ArchiveItems,
    store: &crate::store::Store,
    session_id: &str,
) -> anyhow::Result<()> {
    for (section, items) in items {
        let previous = l.archived.get(&section);
        let content = json!({"section": section, "previous_artifact": previous.map(|p| &p.artifact), "items": items});
        let artifact = store.put_artifact(
            Some(session_id),
            &format!("Task ledger archive: {section}"),
            content.to_string().as_bytes(),
        )?;
        let count = previous.map_or(0, |p| p.count) + items.len() as u64;
        l.archived
            .insert(section, tessera_proto::LedgerArchive { count, artifact });
    }
    Ok(())
}

/// Migrate old oversized ledgers before any engine injects them into a request.
pub(crate) fn maintain_ledger(
    l: &mut TaskLedger,
    store: &crate::store::Store,
    session_id: &str,
) -> anyhow::Result<bool> {
    let mut archived = ArchiveItems::new();
    bound_ledger(l, &mut archived);
    if archived.is_empty() {
        return Ok(false);
    }
    save_archives(l, archived, store, session_id)?;
    Ok(true)
}

pub(crate) fn ledger_version(ledger: Option<&TaskLedger>) -> String {
    crate::util::sha256_hex(
        serde_json::to_string(&ledger.cloned().unwrap_or_default())
            .expect("ledger serializes")
            .as_bytes(),
    )
}

pub(crate) fn unchanged_ledger(version: &str) -> String {
    // Comparison/persistence uses the full hash; display only its short id.
    format!("[Tessera work state unchanged (version {version:.12})]")
}

/// One line on where the task stands (the `task` tool's confirmation).
fn ledger_status(l: &TaskLedger) -> String {
    let mut parts = Vec::new();
    if !l.plan.is_empty() {
        let done = l
            .plan
            .iter()
            .filter(|p| matches!(p.status, PlanStatus::Done | PlanStatus::Skipped))
            .count();
        let mut s = format!("Plan {done}/{} done", l.plan.len());
        if let Some((i, p)) = l
            .plan
            .iter()
            .enumerate()
            .find(|(_, p)| !matches!(p.status, PlanStatus::Done | PlanStatus::Skipped))
        {
            s.push_str(&format!(
                "; next: {}. {}",
                i + 1,
                crate::util::one_line(&p.text, 100)
            ));
        }
        parts.push(s);
    }
    if !l.done_criteria.is_empty() {
        let met = l
            .criteria
            .iter()
            .filter(|c| c.status == CriterionStatus::Met)
            .count();
        parts.push(format!("Done when: {met}/{} met", l.done_criteria.len()));
    }
    if parts.is_empty() {
        "Call `task` with no arguments to read it.".into()
    } else {
        parts.join(". ") + "."
    }
}

fn parse_criterion(s: &str) -> CriterionStatus {
    match s.trim().to_ascii_lowercase().as_str() {
        "met" | "done" | "pass" | "passed" | "yes" => CriterionStatus::Met,
        "unmet" | "not_met" | "failed" | "fail" | "no" => CriterionStatus::Unmet,
        "unverifiable" | "unknown" | "unverified" => CriterionStatus::Unverifiable,
        _ => CriterionStatus::Unchecked,
    }
}

pub struct Task;

impl Tool for Task {
    fn name(&self) -> &'static str {
        "task"
    }
    fn description(&self) -> String {
        "Ledger. Lists replace; add_* appends; remove archives decisions/artifacts/notes. \
         update/verify/remove: 1-based. No args: read."
            .into()
    }
    fn schema(&self) -> Value {
        let list = json!({"type": "array", "items": {"type": "string"}});
        json!({
            "type": "object",
            "properties": {
                "goal": {"type": "string"},
                "constraints": list,
                "plan": {"type": "array", "items": {"type": "object", "properties": {
                    "text": {"type": "string"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "done", "blocked", "skipped"]}
                }, "required": ["text"]}},
                "update": {"type": "array", "items": {"type": "object", "properties": {
                    "index": {"type": "integer"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "done", "blocked", "skipped"]}
                }, "required": ["index", "status"]}},
                "decisions": list,
                "artifacts": list,
                "notes": list,
                "remove": {"type": "object", "additionalProperties": {"type": "array", "items": {"type": "integer", "minimum": 1}}},
                "add_decisions": list,
                "open_questions": list,
                "add_artifacts": list,
                "add_notes": list,
                "done_criteria": list,
                "verify": {"type": "array", "items": {"type": "object", "properties": {
                    "index": {"type": "integer"},
                    "status": {"type": "string", "enum": ["met", "unmet", "unverifiable"]},
                    "evidence": {"type": "string"}
                }, "required": ["index", "status", "evidence"]}}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        if let Some(g) = opt_str(args, "goal") {
            return crate::util::one_line(g, 80);
        }
        if let Some(u) = args.get("update").and_then(Value::as_array) {
            return format!("{} plan updates", u.len());
        }
        if let Some(v) = args.get("verify").and_then(Value::as_array) {
            return format!("{} criteria checked", v.len());
        }
        "ledger update".into()
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let mut l = ctx.host.ledger(&ctx.session_id).unwrap_or_default();
            let changes = args
                .as_object()
                .is_some_and(|o| o.keys().any(|k| LEDGER_FIELDS.contains(&k.as_str())));
            if !changes {
                match maintain_ledger(&mut l, ctx.host.store(), &ctx.session_id) {
                    Ok(true) => {
                        if let Err(e) = ctx.host.set_ledger(&ctx.session_id, l.clone()) {
                            return ToolOutput::error(format!("Could not save ledger: {e}"));
                        }
                    }
                    Err(e) => return ToolOutput::error(format!("Could not archive ledger: {e}")),
                    Ok(false) => {}
                }
                return ToolOutput::text(format!("Task ledger:\n{}", render_ledger(&l)));
            }
            if let Some(g) = opt_str(&args, "goal") {
                l.goal = g.trim().to_string();
            }
            if let Some(v) = args.get("constraints").and_then(str_list) {
                l.constraints = v;
            }
            if let Some(items) = args.get("plan").and_then(Value::as_array) {
                l.plan = items
                    .iter()
                    .filter_map(|i| {
                        let (text, status) = match i {
                            Value::String(s) => (s.clone(), PlanStatus::Pending),
                            _ => (
                                i.get("text")?.as_str()?.to_string(),
                                i.get("status")
                                    .and_then(Value::as_str)
                                    .map(parse_status)
                                    .unwrap_or(PlanStatus::Pending),
                            ),
                        };
                        Some(PlanItem { text, status })
                    })
                    .collect();
            }
            let mut errors = Vec::new();
            let mut archived = ArchiveItems::new();
            for (name, list) in [
                ("decisions", &mut l.decisions),
                ("artifacts", &mut l.artifacts),
                ("notes", &mut l.notes),
            ] {
                if let Some(new) = args.get(name).and_then(str_list) {
                    replace_list(name, list, new, &mut archived);
                }
                if let Some(remove) = args
                    .get("remove")
                    .and_then(|r| r.get(name))
                    .and_then(Value::as_array)
                {
                    let mut indexes: Vec<usize> = remove
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|i| i as usize)
                        .collect();
                    indexes.sort_unstable();
                    indexes.dedup();
                    for index in indexes.into_iter().rev() {
                        if index == 0 || index > list.len() {
                            errors.push(format!("{name} item {index} does not exist"));
                        } else {
                            archived
                                .entry(name.into())
                                .or_default()
                                .push(list.remove(index - 1));
                        }
                    }
                }
            }
            if let Some(ups) = args.get("update").and_then(Value::as_array) {
                for u in ups {
                    let idx = u.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let status = u
                        .get("status")
                        .and_then(Value::as_str)
                        .map(parse_status)
                        .unwrap_or(PlanStatus::Pending);
                    match l.plan.get_mut(idx.wrapping_sub(1)) {
                        Some(p) => p.status = status,
                        None => errors.push(format!("plan item {idx} does not exist")),
                    }
                }
            }
            if let Some(v) = args.get("add_decisions").and_then(str_list) {
                l.decisions.extend(v);
            }
            if let Some(v) = args.get("open_questions").and_then(str_list) {
                l.open_questions = v;
            }
            if let Some(v) = args.get("add_artifacts").and_then(str_list) {
                for a in v {
                    if !l.artifacts.contains(&a) {
                        l.artifacts.push(a);
                    }
                }
            }
            if let Some(v) = args.get("add_notes").and_then(str_list) {
                l.notes.extend(v);
            }

            if let Some(v) = args.get("done_criteria").and_then(str_list) {
                // Keep checks of criteria whose text did not change.
                let old: Vec<(String, CriterionState)> = l
                    .done_criteria
                    .iter()
                    .cloned()
                    .zip(l.criteria.iter().cloned())
                    .collect();
                l.criteria = v
                    .iter()
                    .map(|c| {
                        old.iter()
                            .find(|(t, _)| t == c)
                            .map(|(_, s)| s.clone())
                            .unwrap_or(CriterionState {
                                status: CriterionStatus::Unchecked,
                                evidence: String::new(),
                            })
                    })
                    .collect();
                l.done_criteria = v;
            }
            l.criteria.resize(
                l.done_criteria.len(),
                CriterionState {
                    status: CriterionStatus::Unchecked,
                    evidence: String::new(),
                },
            );
            if let Some(checks) = args.get("verify").and_then(Value::as_array) {
                for c in checks {
                    let idx = c.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let status = c
                        .get("status")
                        .and_then(Value::as_str)
                        .map(parse_criterion)
                        .unwrap_or_default();
                    let evidence = c
                        .get("evidence")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    match l.criteria.get_mut(idx.wrapping_sub(1)) {
                        Some(slot) => {
                            if status == CriterionStatus::Met && evidence.is_empty() {
                                errors.push(format!("criterion {idx} marked met without evidence"));
                            }
                            *slot = CriterionState {
                                status,
                                evidence: crate::util::truncate(&evidence, 500),
                            };
                        }
                        None => errors.push(format!("done_criteria item {idx} does not exist")),
                    }
                }
            }
            bound_ledger(&mut l, &mut archived);
            if let Err(e) = validate_live_ledger(&l) {
                return ToolOutput::error(e.to_string());
            }
            let archived_count: usize = archived.values().map(Vec::len).sum();
            if let Err(e) = save_archives(&mut l, archived, ctx.host.store(), &ctx.session_id) {
                return ToolOutput::error(format!(
                    "Could not archive ledger; it was not changed: {e}"
                ));
            }
            if let Err(e) = ctx.host.set_ledger(&ctx.session_id, l.clone()) {
                return ToolOutput::error(format!("Could not save the ledger: {e}"));
            }
            // A short confirmation: the call itself says what changed, and
            // echoing the whole ledger every time filled the context faster
            // than anything else in long tasks.
            let mut text = format!("Ledger saved. {}", ledger_status(&l));
            if archived_count > 0 {
                text.push_str(&format!("\n{archived_count} older items archived losslessly; call task to see recall links and current indexes."));
            }
            if !errors.is_empty() {
                text.push_str(&format!("\nWarnings: {}", errors.join("; ")));
            }
            ToolOutput::text(text)
        })
    }
}

pub struct MemoryTool;

impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> String {
        "Save durable facts across sessions: `user` for the person's preferences/environment/corrections; `memory` for \
         your own lessons and useful facts. Short facts, not progress (`task`) or procedures (`skill_manage`). `list` shows \
         space and entries; add, replace/remove (`old`: entry id or unique substring). `preview:true` checks without \
         saving. Consolidate when full; applies in new sessions."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "enum": ["memory", "user"]},
                "action": {"type": "string", "enum": ["list", "add", "replace", "remove"]},
                "content": {"type": "string"},
                "old": {"type": "string"},
                "preview": {"type": "boolean"}
            },
            "required": ["target", "action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn action_kind(&self, args: &Value) -> ToolKind {
        if opt_str(args, "action") == Some("list") || opt_bool(args, "preview") == Some(true) {
            ToolKind::Read
        } else {
            ToolKind::Write
        }
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        format!(
            "{} {}: {}",
            opt_str(args, "action").unwrap_or("?"),
            opt_str(args, "target").unwrap_or("memory"),
            crate::util::one_line(
                opt_str(args, "content")
                    .or(opt_str(args, "old"))
                    .unwrap_or(""),
                60
            )
        )
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(why) = ctx.authorize_mutation(self, &args) {
                return ToolOutput::error(why);
            }
            let target = if opt_str(&args, "target") == Some("user") {
                MemoryTarget::User
            } else {
                MemoryTarget::Memory
            };
            let memory = ctx.host.memory(&ctx.session_id);
            if opt_str(&args, "action") == Some("list") {
                return ToolOutput::text(memory.describe(target));
            }
            let content = opt_str(&args, "content").unwrap_or("");
            let old = opt_str(&args, "old").unwrap_or("");
            let op = match opt_str(&args, "action").unwrap_or("add") {
                "add" => MemoryOp::Add(content),
                "replace" => MemoryOp::Replace { old, new: content },
                "remove" => MemoryOp::Remove(old),
                other => return ToolOutput::error(format!("Unknown action `{other}`.")),
            };
            if args
                .get("preview")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return match memory.preview(target, op) {
                    Ok(text) => ToolOutput::text(text),
                    Err(e) => ToolOutput::error(format!("Preview rejected — NOT saved: {e}")),
                };
            }
            match ctx
                .admission
                .run(|| memory.apply(target, op).map_err(std::io::Error::other))
            {
                Ok(usage) => ToolOutput::text(format!("Saved ({usage}). No need to repeat it.")),
                Err(e) => ToolOutput::error(format!("Not saved: {e}")),
            }
        })
    }
}

pub struct SkillView;

impl Tool for SkillView {
    fn name(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> String {
        "Load a skill by name (or one of its supporting files with `file`); without a name, lists all skills."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "file": {"type": "string"}}
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let skills = ctx.host.skills(&ctx.cwd);
            let Some(name) = opt_str(&args, "name").filter(|n| !matches!(*n, "list" | "*")) else {
                if skills.is_empty() {
                    return ToolOutput::text("No skills yet.");
                }
                let lines: Vec<String> = skills
                    .iter()
                    .map(|s| {
                        format!(
                            "- {}: {}",
                            s.name,
                            crate::util::truncate(&s.description, 200)
                        )
                    })
                    .collect();
                let (text, _) = super::fit_output(&lines.join("\n"), ctx, "skills");
                return ToolOutput::text(text);
            };
            let Some(skill) = skills.iter().find(|s| s.name == name) else {
                let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                return ToolOutput::error(format!(
                    "No skill `{name}`. Available: {}",
                    names.join(", ")
                ));
            };
            let path = match opt_str(&args, "file") {
                Some(f) => in_skill(skill.dir(), f, ctx),
                None => contained(skill.dir(), &skill.file, ctx).map(|_| skill.file.clone()),
            };
            let path = match path {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(e),
            };
            let text = match read_skill_text(skill.dir(), &path, ctx).await {
                Ok(t) => t,
                Err(e) => return ToolOutput::error(format!("Cannot read {}: {e}", path.display())),
            };
            if opt_str(&args, "file").is_none() {
                ctx.host.skill_used(name);
            }
            // Skills can come from any repository: nothing hidden, and text
            // aimed at overriding the agent is labelled (and the user told).
            let scan = crate::scan::scan(&text);
            let mut out = format!(
                "[skill {name} · {}]\n{}{}",
                path.display(),
                if scan.is_clean() {
                    String::new()
                } else {
                    format!("{}\n", crate::scan::label("skill", &scan))
                },
                crate::util::truncate(&scan.text, 60_000)
            );
            if opt_str(&args, "file").is_none() {
                let others: Vec<String> = ignore::WalkBuilder::new(skill.dir())
                    .max_depth(Some(3))
                    .build()
                    .flatten()
                    .filter(|e| {
                        e.file_type().is_some_and(|t| t.is_file()) && e.path() != skill.file
                    })
                    .filter_map(|e| {
                        e.path()
                            .strip_prefix(skill.dir())
                            .ok()
                            .map(|p| p.display().to_string())
                    })
                    .take(50)
                    .collect();
                if !others.is_empty() {
                    out.push_str(&format!(
                        "\n\nSupporting files (load with file=…): {}",
                        others.join(", ")
                    ));
                }
            }
            if scan.is_clean() {
                ToolOutput::text(out)
            } else {
                ToolOutput::text(out).with_details(json!({
                    "injection": format!("skill `{name}` ({}): {}", skill.source, scan.summary())
                }))
            }
        })
    }
}

/// `rel` inside a skill folder (see [`contained`]).
fn in_skill(dir: &Path, rel: &str, ctx: &ToolCtx) -> Result<PathBuf, String> {
    if rel.contains("..") || Path::new(rel).is_absolute() {
        return Err("`file` must be a relative path inside the skill folder.".into());
    }
    let p = dir.join(rel);
    contained(dir, &p, ctx)?;
    Ok(p)
}

/// Project skills come from whatever repository the agent is working in, so
/// a symlink in one must not lead out of its folder (into Tessera's own
/// settings, say, which these tools would otherwise read and write).
fn contained(dir: &Path, p: &Path, ctx: &ToolCtx) -> Result<(), String> {
    if !super::canonical(p).starts_with(super::canonical(dir)) {
        return Err("That path leads out of the skill folder (through a symlink).".into());
    }
    // A project's skills follow its folder's rules (read-only stays so).
    match super::files::path_write_risk(p, ctx) {
        Risk::Block { reason } => Err(reason),
        _ => Ok(()),
    }
}

async fn read_skill_text(dir: &Path, path: &Path, ctx: &ToolCtx) -> Result<String, String> {
    let opened = super::file_object::Opened::open(path).map_err(|e| e.to_string())?;
    if !opened.path.starts_with(super::canonical(dir)) {
        return Err("That path leads out of the skill folder.".into());
    }
    let ctx = ctx.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        super::files::authorized_opened_bytes(&opened, &ctx, 16 * 1024 * 1024)
    })
    .await
    .map_err(|e| e.to_string())??;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

const SKILL_CHANGES: [&str; 4] = ["create", "update", "write_file", "delete"];

pub struct SkillManage;

impl Tool for SkillManage {
    fn name(&self) -> &'static str {
        "skill_manage"
    }
    fn description(&self) -> String {
        "Save or improve a skill: a verified multi-step procedure that will clearly recur (exact commands, \
         pitfalls). Rare; never for simple tasks, one-offs or single facts (`memory`). Decide yourself, don't \
         ask. Prefer `update` to a near-duplicate. Actions: `create` (name as-lowercase-dashes, one-line \
         description of what and when, content = steps), `update` (`old`→`new`, or `content`), `write_file` \
         (`file`), `delete`. `scope`: user (default) or project."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "update", "write_file", "delete"]},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "content": {"type": "string"},
                "old": {"type": "string"},
                "new": {"type": "string"},
                "file": {"type": "string"},
                "scope": {"type": "string", "enum": ["user", "project"]}
            },
            "required": ["action", "name"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn action_kind(&self, _args: &Value) -> ToolKind {
        ToolKind::Write
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    /// A skill is instructions later chats load: changing one asks, as
    /// writing into the skills folder does (not in YOLO, and not for the
    /// memory review, which runs on its own).
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let action = opt_str(args, "action").unwrap_or("");
        if !SKILL_CHANGES.contains(&action) {
            return Risk::None;
        }
        if opt_str(args, "scope") == Some("project")
            && let r @ Risk::Block { .. } =
                super::files::path_write_risk(&ctx.cwd.join(".tessera/skills"), ctx)
        {
            return r;
        }
        Risk::Ask {
            reason: format!(
                "changes a Tessera skill (instructions later chats load): {}",
                opt_str(args, "name").unwrap_or("").trim()
            ),
            key: "write:skills".into(),
        }
    }
    fn summary(&self, args: &Value) -> String {
        crate::util::one_line(
            &format!(
                "{} skill {}",
                opt_str(args, "action").unwrap_or(""),
                opt_str(args, "name").unwrap_or("")
            ),
            120,
        )
    }
    /// What the skill would say: the person approving reads it in full.
    fn approval_summary(&self, args: &Value) -> String {
        let s = |k: &str| opt_str(args, k).unwrap_or("").trim();
        let mut out = self.summary(args);
        for (k, label) in [("description", ": "), ("file", ", file ")] {
            if !s(k).is_empty() {
                out.push_str(label);
                out.push_str(s(k));
            }
        }
        for k in ["old", "new", "content"] {
            if !s(k).is_empty() {
                out.push_str(&format!("\n\n{k}:\n{}", s(k)));
            }
        }
        out
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(why) = ctx.authorize_mutation(self, &args) {
                return ToolOutput::error(why);
            }
            let action = opt_str(&args, "action").unwrap_or("");
            let name = match arg_str(&args, "name") {
                Ok(n) => n.trim().to_ascii_lowercase().replace([' ', '_'], "-"),
                Err(e) => return e,
            };
            if name.is_empty() || name.contains(['/', '\\', '.']) {
                return ToolOutput::error("Skill names are lowercase words joined by dashes.");
            }
            for key in ["description", "content", "new"] {
                let hidden = opt_str(&args, key).map_or(0, |t| crate::scan::strip_hidden(t).1);
                if hidden > 0 {
                    return ToolOutput::error(format!(
                        "`{key}` contains {hidden} hidden Unicode characters; write skills as plain visible text."
                    ));
                }
            }
            let all = ctx.host.skills(&ctx.cwd);
            let existing = all.iter().find(|s| s.name == name).cloned();
            let project = opt_str(&args, "scope") == Some("project");
            let root = if project {
                ctx.cwd.join(".tessera/skills")
            } else {
                ctx.host.user_skills_dir()
            };
            let dir = existing
                .as_ref()
                .map(|s| s.dir().to_path_buf())
                .unwrap_or_else(|| root.join(&name));
            let skill_md = dir.join("SKILL.md");
            let result: Result<String, String> = async {
                match action {
                    "create" => {
                        if existing.is_some() {
                            return Err(format!("Skill `{name}` exists; use update."));
                        }
                        contained(&root, &skill_md, ctx)?;
                        let desc =
                            opt_str(&args, "description").ok_or("`description` is required")?;
                        let body = opt_str(&args, "content").ok_or("`content` is required")?;
                        // The agent decides what is worth a skill; these are
                        // the checks for when it decides badly.
                        let evidence = ctx.host.skill_evidence(&ctx.session_id);
                        if !evidence.user_asked
                            && ctx.host.skills_made_this_turn(&ctx.session_id) > 0
                        {
                            return Err(
                                "Not saved: one new skill per turn is plenty. Carry on.".into()
                            );
                        }
                        crate::skills::worth_saving(
                            &name,
                            desc,
                            body,
                            &all,
                            if project { "project" } else { "user" },
                            evidence,
                        )?;
                        let text = format!(
                            "---\nname: {name}\ndescription: {}\n---\n\n{}\n",
                            desc.replace('\n', " "),
                            body.trim()
                        );
                        ctx.admission
                            .run(|| {
                                std::fs::create_dir_all(&dir)?;
                                std::fs::write(&skill_md, text)
                            })
                            .map_err(|e| e.to_string())?;
                        Ok(format!("Created skill `{name}` at {}", skill_md.display()))
                    }
                    "update" => {
                        let s = existing.as_ref().ok_or(format!("No skill `{name}`."))?;
                        contained(s.dir(), &s.file, ctx)?;
                        let current = read_skill_text(s.dir(), &s.file, ctx)
                            .await
                            .map_err(|e| e.to_string())?;
                        let next = if let (Some(old), Some(new)) = (
                            opt_str(&args, "old"),
                            args.get("new").and_then(Value::as_str),
                        ) {
                            if current.matches(old).count() != 1 {
                                return Err("`old` must match exactly once in SKILL.md.".into());
                            }
                            current.replacen(old, new, 1)
                        } else if let Some(body) = opt_str(&args, "content") {
                            let (fm, _) = crate::skills::frontmatter(&current);
                            let mut fm = fm;
                            if let Some(d) = opt_str(&args, "description") {
                                fm = fm
                                    .lines()
                                    .map(|l| {
                                        if l.starts_with("description:") {
                                            format!("description: {d}")
                                        } else {
                                            l.to_string()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                            }
                            format!("---{fm}\n---\n\n{}\n", body.trim())
                        } else {
                            return Err("Provide old/new or content.".into());
                        };
                        ctx.admission
                            .run(|| std::fs::write(&s.file, next))
                            .map_err(|e| e.to_string())?;
                        Ok(format!("Updated skill `{name}`."))
                    }
                    "write_file" => {
                        let f = opt_str(&args, "file").ok_or("`file` is required")?;
                        if existing.is_none() {
                            return Err(format!("Create skill `{name}` first."));
                        }
                        let p = in_skill(&dir, f, ctx)?;
                        ctx.admission
                            .run(|| {
                                if let Some(parent) = p.parent() {
                                    std::fs::create_dir_all(parent)?;
                                }
                                std::fs::write(&p, opt_str(&args, "content").unwrap_or(""))
                            })
                            .map_err(|e| e.to_string())?;
                        Ok(format!("Wrote {}", p.display()))
                    }
                    "delete" => {
                        let s = existing.as_ref().ok_or(format!("No skill `{name}`."))?;
                        contained(s.dir(), s.dir(), ctx)?;
                        ctx.admission
                            .run(|| std::fs::remove_dir_all(s.dir()))
                            .map_err(|e| e.to_string())?;
                        Ok(format!("Deleted skill `{name}`."))
                    }
                    other => Err(format!("Unknown action `{other}`.")),
                }
            }
            .await;
            match result {
                Ok(t) => ToolOutput::text(t),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

pub struct Recall;

impl Tool for Recall {
    fn name(&self) -> &'static str {
        "recall"
    }
    fn description(&self) -> String {
        "Past conversations and details no longer in your context: `query` full-text search (this chat, or \
         `scope: all`; `since`: date or 7d/24h), history by `seq` (`to`/`around` for more; `session_id` for another \
         chat), or a stored `artifact` by `offset` or `grep`."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "scope": {"type": "string", "enum": ["session", "all"]},
                "since": {"type": "string"},
                "seq": {"type": "integer"},
                "to": {"type": "integer"},
                "around": {"type": "integer"},
                "session_id": {"type": "string"},
                "artifact": {"type": "string"},
                "offset": {"type": "integer"},
                "grep": {"type": "string"}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let store = ctx.host.store();
            // Never private chats or helper sessions; a bot with its own
            // memory only its own conversations (separation of duties).
            let may = |other: &str| ctx.host.may_recall(&ctx.session_id, other);
            if let Some(id) = opt_str(&args, "artifact") {
                let owner = store.artifact_session(id).ok().flatten();
                let mine = owner.as_deref().is_none_or(may);
                let Ok(Some((label, content))) = store.artifact(id).map(|a| a.filter(|_| mine))
                else {
                    return ToolOutput::error(format!("No artifact `{id}`."));
                };
                let text = String::from_utf8_lossy(&content).to_string();
                if let Some(pat) = opt_str(&args, "grep") {
                    let re = match regex::Regex::new(pat) {
                        Ok(r) => r,
                        Err(e) => return ToolOutput::error(format!("Invalid regex: {e}")),
                    };
                    let hits: Vec<String> = text
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| re.is_match(l))
                        .take(300)
                        .map(|(i, l)| format!("{}:{}", i + 1, crate::util::truncate(l, 400)))
                        .collect();
                    return scanned(format!(
                        "[{id} · {label}] {} matching lines\n{}",
                        hits.len(),
                        hits.join("\n")
                    ));
                }
                let offset = opt_u64(&args, "offset").unwrap_or(0) as usize;
                let start = crate::util::ceil_char(&text, offset.min(text.len()));
                let end = crate::util::floor_char(&text, (start + 30_000).min(text.len()));
                let mut out = format!(
                    "[{id} · {label}] bytes {start}-{end} of {}\n{}",
                    text.len(),
                    &text[start..end]
                );
                if end < text.len() {
                    out.push_str(&format!("\n[continue with offset={end}]"));
                }
                return scanned(out);
            }
            let session = opt_str(&args, "session_id")
                .unwrap_or(&ctx.session_id)
                .to_string();
            if let Some(why) = ctx.host.recall_refusal(&ctx.session_id, &session) {
                return ToolOutput::error(why);
            }
            if let Some(seq) = opt_u64(&args, "seq") {
                let around = opt_u64(&args, "around").unwrap_or(0).min(20);
                let (first, last) = match opt_u64(&args, "to") {
                    Some(to) if to > seq => (seq, to.min(seq + 39)),
                    _ => (seq.saturating_sub(around), seq + around),
                };
                if first == last {
                    return match store.entry(&session, seq) {
                        Ok(Some(e)) if !is_protocol_entry(&e) => {
                            scanned(crate::util::truncate(&entry_text(&e.body), 60_000))
                        }
                        _ => ToolOutput::error(format!("No history item {seq}.")),
                    };
                }
                let window: Vec<_> = store
                    .entries(&session, first.saturating_sub(1))
                    .unwrap_or_default()
                    .into_iter()
                    .take_while(|e| e.seq <= last)
                    .filter(|e| {
                        !matches!(e.body, EntryBody::Notice { .. }) && !is_protocol_entry(e)
                    })
                    .collect();
                if !window.iter().any(|e| e.seq == seq) {
                    return ToolOutput::error(format!("No history item {seq}."));
                }
                // Neighbours are context: shorter than the item asked for.
                let ranged = opt_u64(&args, "to").is_some();
                let per = (40_000 / window.len().max(1)).clamp(1_000, 12_000);
                let text: Vec<String> = window
                    .iter()
                    .map(|e| {
                        let asked = e.seq == seq && !ranged;
                        format!(
                            "── seq {}{} ──\n{}",
                            e.seq,
                            if asked { " (requested)" } else { "" },
                            crate::util::truncate(
                                &entry_text(&e.body),
                                if asked { 20_000 } else { per }
                            )
                        )
                    })
                    .collect();
                return scanned(text.join("\n\n"));
            }
            let Some(q) = opt_str(&args, "query") else {
                return ToolOutput::error("Provide query, seq or artifact.");
            };
            let since = match opt_str(&args, "since") {
                Some(s) => match parse_since(s, crate::util::now_ms()) {
                    Some(t) => Some(t),
                    None => {
                        return ToolOutput::error(format!(
                            "`since` must be a date like 2026-09-01 (optionally with a time, 2026-09-01T14:30) or an age like 7d, 24h, 90m; got `{s}`."
                        ));
                    }
                },
                None => None,
            };
            let scope = if opt_str(&args, "scope") == Some("all") {
                None
            } else {
                Some(session.as_str())
            };
            let hits = store.search_since(q, scope, since, 60).map(|hits| {
                hits.into_iter()
                    .filter(|(h, _)| scope.is_some() || may(&h.session_id))
                    .filter(|(h, _)| {
                        store
                            .entry(&h.session_id, h.seq)
                            .ok()
                            .flatten()
                            .is_some_and(|e| !is_protocol_entry(&e))
                    })
                    .take(15)
                    .collect::<Vec<_>>()
            });
            let scope_name = if scope.is_some() { "session" } else { "all" };
            match hits {
                Ok(hits) if hits.is_empty() => ToolOutput::text("No matches.")
                    .with_details(json!({"recall": {"query": q, "scope": scope_name, "hits": []}})),
                Ok(hits) => {
                    // For the apps: each hit, linkable to its chat.
                    let listed: Vec<Value> = hits
                        .iter()
                        .map(|(h, ts)| {
                            json!({
                                "session_id": h.session_id,
                                "session_title": h.session_title,
                                "seq": h.seq,
                                "ts": ts,
                                "snippet": crate::scan::strip_hidden(&h.snippet).0,
                            })
                        })
                        .collect();
                    let mut out = scanned(
                        hits.iter()
                            .map(|(h, ts)| {
                                let where_ = if scope.is_some() {
                                    String::new()
                                } else {
                                    let day = crate::jobs::fmt_time(*ts);
                                    let day = day.split(' ').next().unwrap_or("");
                                    format!(" [{} · {day} · {}]", h.session_title, h.session_id)
                                };
                                format!("seq {}{where_}: {}", h.seq, h.snippet.replace('\n', " "))
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                            + "\n(seq=N reads an item, with to=M or around=N for more; add session_id for another chat)",
                    );
                    let mut details = out.details.take().unwrap_or_else(|| json!({}));
                    details["recall"] = json!({"query": q, "scope": scope_name, "hits": listed});
                    out.with_details(details)
                }
                Err(e) => ToolOutput::error(e.to_string()),
            }
        })
    }
}

/// Recalled text is data: hidden characters go, instruction-like text is
/// labelled, and the user is told (hidden characters alone are common in
/// pages read earlier, and not worth a warning).
fn scanned(text: String) -> ToolOutput {
    let scan = crate::scan::scan_page(&text);
    if !scan.has_instructions() {
        return ToolOutput::text(scan.text);
    }
    ToolOutput::text(format!(
        "{}\n{}",
        crate::scan::label("history", &scan),
        scan.text
    ))
    .with_details(json!({"injection": format!("recalled history: {}", scan.instructions())}))
}

/// `7d`, `24h`, `90m`, `2w` ago, or a local date/time (`2026-09-01`,
/// `2026-09-01T14:30`, `2026-09-01 14:30:05`, RFC 3339), as unix ms.
pub fn parse_since(s: &str, now_ms: u64) -> Option<u64> {
    let s = s.trim();
    if let Some(unit) = s.chars().last().filter(|c| c.is_ascii_alphabetic())
        && let Ok(n) = s[..s.len() - 1].trim().parse::<u64>()
    {
        let mins = match unit.to_ascii_lowercase() {
            'm' => 1,
            'h' => 60,
            'd' => 60 * 24,
            'w' => 60 * 24 * 7,
            _ => return None,
        };
        return Some(now_ms.saturating_sub(n.saturating_mul(mins * 60_000)));
    }
    use time::macros::format_description as fd;
    if let Ok(t) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return Some((t.unix_timestamp_nanos() / 1_000_000).max(0) as u64);
    }
    let s = s.replacen('T', " ", 1);
    let local = |dt: time::PrimitiveDateTime| crate::jobs::local_datetime(dt).ok();
    if let Ok(dt) =
        time::PrimitiveDateTime::parse(&s, fd!("[year]-[month]-[day] [hour]:[minute]:[second]"))
    {
        return local(dt);
    }
    if let Ok(dt) = time::PrimitiveDateTime::parse(&s, fd!("[year]-[month]-[day] [hour]:[minute]"))
    {
        return local(dt);
    }
    if let Ok(d) = time::Date::parse(&s, fd!("[year]-[month]-[day]")) {
        return local(d.midnight());
    }
    None
}

pub fn entry_text(body: &EntryBody) -> String {
    use tessera_proto::AssistantPart;
    match body {
        EntryBody::User { content, .. } => {
            format!("[user]\n{}", tessera_proto::plain_text(content))
        }
        EntryBody::Assistant { content, .. } => {
            let mut s = String::from("[assistant]\n");
            for p in content {
                match p {
                    AssistantPart::Text { text } => s.push_str(text),
                    AssistantPart::ToolCall {
                        name, arguments, ..
                    } => s.push_str(&format!("\n→ {name} {arguments}")),
                    AssistantPart::Thinking { .. } => {}
                }
            }
            s
        }
        EntryBody::ToolResult {
            tool,
            content,
            is_error,
            ..
        } => {
            format!(
                "[{tool} result{}]\n{}",
                if *is_error { " (error)" } else { "" },
                tessera_proto::plain_text(content)
            )
        }
        EntryBody::Compaction { summary, .. } => format!("[compaction summary]\n{summary}"),
        EntryBody::Notice { text, .. } => format!("[notice] {text}"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct SubTask {
    pub name: String,
    pub prompt: String,
    pub readonly: bool,
    /// Model to run it on (`provider/model`, part of a model's name, `@role`).
    pub model: Option<String>,
    /// Agent definition to run as (see `crate::agents`).
    pub agent: Option<String>,
    /// Working folder, within the parent's existing folder scope.
    pub cwd: Option<String>,
    /// Requested report format (see `crate::agents::parse_output`).
    pub output: Option<Value>,
    /// Work in a private git worktree; changes come back as a patch.
    pub worktree: bool,
    /// OpenAI's Daybreak Blue for this helper; None inherits the chat's choice.
    pub daybreak: Option<bool>,
}

/// A subagent the host started in the background.
#[derive(Debug, Clone)]
pub struct Started {
    pub name: String,
    pub session_id: String,
    /// Something the parent should know (e.g. a definition's model fell back).
    pub note: Option<String>,
}

/// `agents` actions the host carries out.
#[derive(Debug, Clone)]
pub enum SubAction {
    /// Continue a finished subagent with a follow-up.
    Message {
        which: String,
        text: String,
    },
    /// Apply an isolated subagent's changes to the parent's files; `seq` is
    /// the parent's assistant entry (file snapshots, for rewinds).
    Apply {
        which: String,
        seq: u64,
    },
    Discard {
        which: String,
    },
    /// Answer the approval one waits for (see `engine/helper_approvals.rs`);
    /// `pick` names it (its id from the note), else the oldest.
    Approve {
        which: String,
        pick: Option<String>,
    },
    Decline {
        which: String,
        pick: Option<String>,
    },
}

pub struct Agent;

/// Subagents per `agent` call; beyond the concurrency limit they queue.
const MAX_TASKS: usize = 32;

/// The `agent` description before its list of agent types (a lean start
/// swaps in a shorter one, `profiles::lean_start_specs`).
pub const AGENT_INTRO: &str = "Delegate independent research or non-overlapping edits. Give self-contained prompts and required results; \
     helpers see none of this chat. BACKGROUND by default: accepted tasks may queue; reports arrive here and you resume \
     when all finish. Tell the user; don't poll. `wait:true` blocks you and the user, so use only when asked. Per task: \
     `agent` picks a type; `output` requests JSON; `cwd` selects an allowed folder; \
     `isolation:\"worktree\"` isolates edits for later `agents` apply. Start/status disclose effective tools.";

impl Agent {
    /// The description, listing the agent definitions available (computed
    /// once per session, so it stays byte-stable for prompt caching).
    pub fn describe(defs: &[crate::agents::AgentDef]) -> String {
        let mut out = String::from(AGENT_INTRO);
        if defs.is_empty() {
            return out;
        }
        out.push_str("\nTypes: ");
        const BUDGET: usize = 700;
        let mut used = 0;
        let mut rest = Vec::new();
        for (i, d) in defs.iter().enumerate() {
            let item = format!(
                "{}{} ({})",
                if i == 0 { "" } else { "; " },
                d.name,
                crate::util::truncate(d.description.trim_end_matches('.'), 100)
            );
            if used + item.len() > BUDGET {
                rest.push(d.name.as_str());
                continue;
            }
            used += item.len();
            out.push_str(&item);
        }
        if !rest.is_empty() {
            out.push_str(&format!("; also: {}", rest.join(", ")));
        }
        out.push('.');
        out
    }
}

/// A task from the `agent` call. Settings given next to `tasks` instead of
/// on each task (a common slip) apply to every task that doesn't set them.
fn parse_task(i: usize, t: &Value, call: &Value) -> Option<SubTask> {
    let get = |k: &str| {
        t.get(k)
            .filter(|v| !v.is_null())
            .or_else(|| call.get(k).filter(|v| !v.is_null()))
    };
    let text = |k: &str| {
        get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(SubTask {
        name: opt_str(t, "name")
            .map(str::to_string)
            .unwrap_or_else(|| format!("task {}", i + 1)),
        prompt: t.get("prompt")?.as_str()?.to_string(),
        readonly: get("readonly")
            .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
            .unwrap_or(false),
        model: text("model"),
        cwd: text("cwd"),
        agent: text("agent"),
        output: get("output").and_then(crate::agents::parse_output),
        worktree: matches!(get("isolation"), Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("worktree"))
            || get("isolation") == Some(&Value::Bool(true)),
        daybreak: get("daybreak").and_then(Value::as_bool),
    })
}

impl Tool for Agent {
    fn name(&self) -> &'static str {
        "agent"
    }
    fn description(&self) -> String {
        Self::describe(&crate::agents::builtins())
    }
    fn schema(&self) -> Value {
        // `name` is required on purpose: servers that constrain tool calls
        // with a grammar (llama.cpp) take required keys first, then optional
        // ones only in alphabetical order, so a model that writes `name`
        // after `prompt` could no longer add `agent` or `isolation`.
        json!({
            "type": "object",
            "properties": {
                "tasks": {"type": "array", "items": {"type": "object", "properties": {
                    "name": {"type": "string"},
                    "prompt": {"type": "string"},
                    "agent": {"type": "string"},
                    "cwd": {"type": "string", "description": "Task folder inside the chat or configured allowed scope; default inherits"},
                    "readonly": {"type": "boolean"},
                    "model": {"type": "string"},
                    "output": {"description": "JSON schema or format description"},
                    "isolation": {"type": "string", "enum": ["none", "worktree"]},
                    "daybreak": {"type": "boolean"}
                }, "required": ["name", "prompt"]}},
                "wait": {"type": "boolean"}
            },
            "required": ["tasks"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        let n = args
            .get("tasks")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        format!("{n} subagent{}", if n == 1 { "" } else { "s" })
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let tasks: Vec<SubTask> = args
                .get("tasks")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .enumerate()
                        .filter_map(|(i, t)| parse_task(i, t, &args))
                        .collect()
                })
                .unwrap_or_default();
            if tasks.is_empty() {
                return ToolOutput::error("Provide at least one task with a prompt.");
            }
            if tasks.len() > MAX_TASKS {
                return ToolOutput::error(format!("At most {MAX_TASKS} subagents per call."));
            }
            let names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
            // Subagents of subagents always run in the foreground.
            let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false) || ctx.depth > 0;
            if !wait {
                return match ctx
                    .host
                    .spawn_subagents(&ctx.session_id, &ctx.call_id, tasks)
                {
                    Ok(started) => {
                        let list: Vec<String> = started
                            .iter()
                            .map(|s| {
                                let note = s
                                    .note
                                    .as_deref()
                                    .map(|n| format!(" — {n}"))
                                    .unwrap_or_default();
                                format!("- {} (session {}){note}", s.name, s.session_id)
                            })
                            .collect();
                        let details: Vec<Value> = started
                            .iter()
                            .map(|s| json!({"name": s.name, "session_id": s.session_id, "background": true}))
                            .collect();
                        ToolOutput::text(format!(
                            "Accepted {} subagent(s) in the background (queued, not necessarily running):\n{}\n\nTheir reports will arrive in this conversation as they finish, and you will be \
                             prompted to continue once all are done. Tell the user what you started; don't wait or poll. Use `agents` to check on or stop them.",
                            started.len(),
                            list.join("\n")
                        ))
                        .with_details(json!({ "subagents": details }))
                    }
                    Err(e) => ToolOutput::error(format!("Could not start the subagents: {e}")),
                };
            }
            (ctx.progress)(&format!(
                "Running {} subagent(s): {}\n",
                names.len(),
                names.join(", ")
            ));
            let results = ctx
                .host
                .run_subagents(&ctx.session_id, &ctx.call_id, tasks, ctx.cancel.clone())
                .await;
            let mut out = String::new();
            let mut failed = 0;
            let mut details = Vec::new();
            for (name, r) in names.iter().zip(results) {
                details
                    .push(json!({"name": name, "session_id": r.session_id, "background": false}));
                let session = r
                    .session_id
                    .as_deref()
                    .map(|id| format!(" (session {id})"))
                    .unwrap_or_default();
                let note = r
                    .note
                    .as_deref()
                    .map(|n| format!("{n}\n"))
                    .unwrap_or_default();
                match r.result {
                    Ok(text) => {
                        out.push_str(&format!("## {name}{session}\n{note}{}\n\n", text.trim()))
                    }
                    Err(e) => {
                        failed += 1;
                        out.push_str(&format!("## {name} (failed){session}\n{note}{e}\n\n"));
                    }
                }
            }
            let (text, _) = super::fit_output(out.trim_end(), ctx, "subagent results");
            ToolOutput {
                is_error: failed == names.len(),
                ..ToolOutput::text(text)
            }
            .with_details(json!({ "subagents": details }))
        })
    }
}

pub struct Agents;

impl Tool for Agents {
    fn name(&self) -> &'static str {
        "agents"
    }
    fn description(&self) -> String {
        "Manage helpers: `status`; `stop` (no name: all); `report` (full); `message` with `text`; \
         `apply`/`discard`: isolated worktree changes; `approve`/`decline`: pending approval (only approve what \
         you could do unasked); `types`/`define` (`docs subagents`). Reports arrive on their own; don't poll."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["status", "stop", "report", "message", "apply", "discard", "types", "define", "approve", "decline"]},
                "name": {"type": "string", "description": "Subagent name or session id."},
                "text": {"type": "string"}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    /// `apply` writes the subagent's changes into this session's files;
    /// `define` saves a type every chat's helpers can run as.
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let action = opt_str(args, "action");
        if !matches!(action, Some("apply" | "define")) {
            return Risk::None;
        }
        match (ctx.host.permission(&ctx.session_id), action) {
            (PermissionMode::ReadOnly, Some("define")) => Risk::Block {
                reason: "This session is read-only; nothing was saved.".into(),
            },
            (_, Some("define")) => {
                let name = opt_str(args, "name").unwrap_or("").trim();
                let effective = crate::agents::parse(
                    opt_str(args, "text").unwrap_or(""),
                    name,
                    crate::agents::Source::User,
                    None,
                )
                .map(|d| {
                    format!(
                        "; {}",
                        agent_type_limits(&d, ctx.host.helper_limits(&ctx.session_id))
                    )
                })
                .unwrap_or_default();
                Risk::Ask {
                    reason: format!(
                        "saves an agent type every chat's helpers can run as: {name}{effective}"
                    ),
                    key: "write:agents".into(),
                }
            }
            (PermissionMode::ReadOnly, _) => Risk::Block {
                reason: "This session is read-only; the changes were not applied.".into(),
            },
            (PermissionMode::Ask, _) => Risk::Ask {
                reason: "applies a subagent's changes to your files".into(),
                key: "agents:apply".into(),
            },
            _ => Risk::None,
        }
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("status");
        match opt_str(args, "name") {
            Some(n) => format!("{action} {n}"),
            None => action.to_string(),
        }
    }
    /// The person approving a new agent type reads it in full.
    fn approval_summary(&self, args: &Value) -> String {
        let mut out = self.summary(args);
        if opt_str(args, "action") == Some("define") {
            out.push_str(&format!(
                "\n\n{}",
                opt_str(args, "text").unwrap_or("").trim()
            ));
        }
        out
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let action = opt_str(&args, "action").unwrap_or("status");
            if action == "start" {
                return Agent.run(args, ctx).await;
            }
            let name = opt_str(&args, "name")
                .map(str::trim)
                .filter(|n| !n.is_empty());
            let need_name = || {
                name.map(str::to_string)
                    .ok_or_else(|| ToolOutput::error(format!("`name` is required for `{action}`.")))
            };
            let sub = match action {
                "types" => {
                    return ToolOutput::text(agent_types(
                        &ctx.host.agent_defs(&ctx.cwd),
                        name,
                        ctx.host.helper_limits(&ctx.session_id),
                    ));
                }
                "define" => return define_agent_type(&args, ctx),
                "stop" => {
                    let n = ctx.host.stop_subagents(&ctx.session_id, name);
                    return if n == 0 {
                        ToolOutput::error("No matching running subagent.")
                    } else {
                        ToolOutput::text(format!(
                            "Stopped {n} subagent(s); their partial notes will arrive as reports."
                        ))
                    };
                }
                "report" => {
                    let which = match need_name() {
                        Ok(n) => n,
                        Err(e) => return e,
                    };
                    return match ctx.host.subagent_report(&ctx.session_id, &which) {
                        Ok(text) => {
                            let (text, _) = super::fit_output(&text, ctx, "subagent report");
                            ToolOutput::text(text)
                        }
                        Err(e) => ToolOutput::error(e),
                    };
                }
                "message" => {
                    let which = match need_name() {
                        Ok(n) => n,
                        Err(e) => return e,
                    };
                    let Some(text) = opt_str(&args, "text").filter(|t| !t.trim().is_empty()) else {
                        return ToolOutput::error(
                            "`text` is required for `message`: the follow-up instruction.",
                        );
                    };
                    SubAction::Message {
                        which,
                        text: text.to_string(),
                    }
                }
                "apply" => match need_name() {
                    Ok(which) => SubAction::Apply {
                        which,
                        seq: ctx.seq,
                    },
                    Err(e) => return e,
                },
                "discard" => match need_name() {
                    Ok(which) => SubAction::Discard { which },
                    Err(e) => return e,
                },
                "approve" => match need_name() {
                    Ok(which) => SubAction::Approve {
                        which,
                        pick: opt_str(&args, "text").map(str::to_string),
                    },
                    Err(e) => return e,
                },
                "decline" => match need_name() {
                    Ok(which) => SubAction::Decline {
                        which,
                        pick: opt_str(&args, "text").map(str::to_string),
                    },
                    Err(e) => return e,
                },
                _ => return ToolOutput::text(ctx.host.subagent_status(&ctx.session_id)),
            };
            match ctx.host.subagent_control(&ctx.session_id, sub).await {
                Ok(text) => ToolOutput::text(text),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

/// `agents types`: every type available here with what it may do, or one
/// type's instructions too. The user's own types live in Tessera's settings
/// folder, which tools never open: this is how an agent sees them.
fn agent_types(defs: &[crate::agents::AgentDef], name: Option<&str>, limits: (u32, u32)) -> String {
    use crate::agents::Source;
    let line = |d: &crate::agents::AgentDef| {
        let source = match d.source {
            Source::Builtin => "built in",
            Source::User => "the user's",
            Source::Project => "this project's",
        };
        let tools = d
            .tools
            .as_ref()
            .map_or("all of the chat's".to_string(), |t| t.join(", "));
        format!(
            "- {} ({source}): {}\n  tools: {tools}; read-only: {}; model: {}; {}",
            d.name,
            d.description,
            if d.readonly { "yes" } else { "no" },
            d.model.as_deref().unwrap_or("the chat's"),
            agent_type_limits(d, limits),
        )
    };
    match name {
        Some(n) => {
            let wanted = crate::agents::normalize_name(n).unwrap_or_default();
            match defs.iter().find(|d| d.name == wanted) {
                Some(d) => format!("{}\n\nInstructions:\n{}", line(d), d.prompt),
                None => format!(
                    "No agent type `{n}` here. Available: {}.",
                    defs.iter()
                        .map(|d| d.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        None => {
            let mut out: Vec<String> = defs.iter().map(line).collect();
            out.push("`name` shows one type's instructions.".into());
            out.join("\n")
        }
    }
}

/// Shared by the approval card, saved-type acknowledgement and type list:
/// requested frontmatter cannot raise the user's configured helper limits.
fn agent_type_limits(d: &crate::agents::AgentDef, (steps, minutes): (u32, u32)) -> String {
    let unlimited = |n| if n == 0 { u32::MAX } else { n };
    let (steps, minutes) = (unlimited(steps), unlimited(minutes));
    let (s, m) = d.budget(steps, minutes);
    let shown = |n: u32| {
        if n == u32::MAX {
            "no limit".to_string()
        } else {
            n.to_string()
        }
    };
    format!(
        "effective budget: {} steps, {} min (configured limits: {} steps, {} min); network: {} for sandboxed read-only commands (without a sandbox project code is unconfined)",
        shown(s.unwrap_or(steps)),
        shown(m.unwrap_or(minutes)),
        shown(steps),
        shown(minutes),
        if d.network { "yes" } else { "no" },
    )
}

#[cfg(test)]
mod agent_type_budget_tests {
    use super::agent_type_limits;

    #[test]
    fn definition_limits_narrow_finite_or_unlimited_chat_budgets() {
        for source in [
            crate::agents::Source::User,
            crate::agents::Source::Project,
            crate::agents::Source::Builtin,
        ] {
            let def = crate::agents::parse(
                "---\nname: bounded\nmax_steps: 90\nmax_minutes: 20\n---\nReport findings.",
                "bounded",
                source,
                None,
            )
            .unwrap();
            let finite = agent_type_limits(&def, (2, 3));
            assert!(
                finite.contains("effective budget: 2 steps, 3 min"),
                "{finite}"
            );
            for unlimited in [(0, 0), (u32::MAX, u32::MAX)] {
                let unlimited = agent_type_limits(&def, unlimited);
                assert!(
                    unlimited.contains("effective budget: 90 steps, 20 min"),
                    "{unlimited}"
                );
                assert!(
                    unlimited.contains("configured limits: no limit steps, no limit min"),
                    "{unlimited}"
                );
            }
            let uncapped =
                crate::agents::parse("Report findings.", "uncapped", source, None).unwrap();
            let unlimited = agent_type_limits(&uncapped, (0, 0));
            assert!(
                unlimited.contains("effective budget: no limit steps, no limit min"),
                "{unlimited}"
            );
        }
    }
}
/// `agents define`: save `text` (a definition file, see `docs subagents`)
/// as the user's own type `name`, through Tessera rather than by writing
/// into its settings folder, which stays closed to tools and commands.
fn define_agent_type(args: &Value, ctx: &ToolCtx) -> ToolOutput {
    if ctx.depth > 0 {
        return ToolOutput::error("Helpers can't define agent types; report it to the parent.");
    }
    let Some(name) = opt_str(args, "name").and_then(crate::agents::normalize_name) else {
        return ToolOutput::error(
            "`name` is required for `define`: lowercase words joined by dashes (up to 40 characters).",
        );
    };
    let Some(text) = opt_str(args, "text").filter(|t| !t.trim().is_empty()) else {
        return ToolOutput::error(
            "`text` is required for `define`: the type's file, frontmatter then instructions (`docs subagents`).",
        );
    };
    let hidden = crate::scan::strip_hidden(text).1;
    if hidden > 0 {
        return ToolOutput::error(format!(
            "`text` contains {hidden} hidden Unicode characters; write agent types as plain visible text."
        ));
    }
    let Some(def) = crate::agents::parse(text, &name, crate::agents::Source::User, None) else {
        return ToolOutput::error("That isn't a usable agent type: check its `name`.");
    };
    if def.name != name {
        return ToolOutput::error(format!(
            "The file names the type `{}`; use the same `name` ({name}).",
            def.name
        ));
    }
    let body = crate::skills::frontmatter(text).1;
    if body.trim().is_empty() {
        return ToolOutput::error("Add the type's instructions after the frontmatter.");
    }
    if body.trim().len() > crate::agents::MAX_PROMPT {
        return ToolOutput::error(format!(
            "The instructions are {} characters; keep them under {}.",
            body.trim().len(),
            crate::agents::MAX_PROMPT
        ));
    }
    let path = ctx.host.user_agents_dir().join(format!("{name}.md"));
    let existed = path.exists();
    let mut file = text.trim().to_string();
    file.push('\n');
    if let Err(e) = crate::config::write_atomic(&path, file.as_bytes(), true) {
        return ToolOutput::error(format!("Could not save the agent type: {e}"));
    }
    let shadowed = ctx
        .host
        .agent_defs(&ctx.cwd)
        .into_iter()
        .find(|d| d.name == name)
        .filter(|d| d.source == crate::agents::Source::Project)
        .map(|_| " Here this project's own type of that name still wins.")
        .unwrap_or_default();
    ToolOutput::text(format!(
        "{} the agent type `{name}` for every chat; {}. Start one with `agent` and `\"agent\": \"{name}\"`.{shadowed}",
        if existed { "Updated" } else { "Saved" },
        agent_type_limits(&def, ctx.host.helper_limits(&ctx.session_id)),
    ))
}

pub struct Schedule;

/// Exact public actions only: never turn a misspelling into a new job.
pub(crate) fn schedule_action(args: &Value) -> Result<&str, &'static str> {
    match opt_str(args, "action") {
        Some(a @ ("list" | "create" | "update" | "delete" | "end")) => Ok(a),
        _ => Err("Unknown schedule action; use list, create, update, delete or end."),
    }
}
/// A job's state and last run, in a line (`schedule list`).
fn job_line(j: &Job) -> String {
    let state = match (&j.ended, j.enabled) {
        (Some(why), false) => format!("ended ({why})"),
        (_, true) => "enabled".into(),
        (_, false) => "paused".into(),
    };
    let mut out = format!("{} · {} · `{}` · {state}", j.id, j.name, j.schedule);
    if let Some(t) = j.last_run {
        out.push_str(&format!(
            " · last run {} {}",
            crate::jobs::fmt_time(t),
            j.last_status.as_deref().unwrap_or("?")
        ));
    }
    if let Some(m) = j.max_runs {
        out.push_str(&format!(" · run {}/{m}", j.runs));
    }
    if let Some(u) = j.until {
        out.push_str(&format!(" · until {}", crate::jobs::fmt_time(u)));
    }
    out.push_str(&format!(" · {}", crate::util::one_line(&j.prompt, 80)));
    out
}

impl Tool for Schedule {
    fn name(&self) -> &'static str {
        "schedule"
    }
    fn description(&self) -> String {
        "Jobs that run a prompt on their own. `schedule`: cron (\"0 9 * * 1-5\" = weekdays 09:00 server time) or \
         `@every 30m`. Actions: list, create, update, delete, end (from a job's run: goal met, no more runs). \
         Temporary checks need `until` (date) or `max_runs` (1 = once)."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "create", "update", "delete", "end"]},
                "id": {"type": "string"},
                "name": {"type": "string"},
                "schedule": {"type": "string"},
                "prompt": {"type": "string", "description": "Self-contained; each run starts fresh."},
                "until": {"type": "string"},
                "max_runs": {"type": "integer"},
                "enabled": {"type": "boolean"},
                "unattended": {"type": "boolean", "description": "No approval prompts (capped by this chat's). Default true."}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn action_kind(&self, args: &Value) -> ToolKind {
        if opt_str(args, "action") == Some("list") {
            ToolKind::Read
        } else {
            ToolKind::Write
        }
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    /// A job runs later with nobody watching, so changing one is the user's
    /// call. Ending its own schedule from a run only means fewer runs.
    fn assess(&self, args: &Value, _ctx: &ToolCtx) -> Risk {
        match schedule_action(args) {
            Ok("list" | "end") => Risk::None,
            Ok(_) => Risk::Ask {
                reason: "changes scheduled jobs, which run later without anyone watching".into(),
                key: "schedule".into(),
            },
            Err(reason) => Risk::Block {
                reason: reason.into(),
            },
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(why) = ctx.authorize_mutation(self, &args) {
                return ToolOutput::error(why);
            }
            let action = match schedule_action(&args) {
                Ok(a) => a,
                Err(why) => return ToolOutput::error(why),
            };
            if action == "list" {
                let jobs = ctx.host.jobs();
                if jobs.is_empty() {
                    return ToolOutput::text("No scheduled jobs.");
                }
                return ToolOutput::text(jobs.iter().map(job_line).collect::<Vec<_>>().join("\n"));
            }
            if action == "end" {
                // Why, if the model says (not in the schema: the run's
                // report says it anyway).
                let reason = opt_str(&args, "reason").unwrap_or("");
                return match ctx.host.end_job_schedule(&ctx.session_id, reason) {
                    Ok(text) => ToolOutput::text(text),
                    Err(e) => ToolOutput::error(e),
                };
            }
            // A bot's duty runs as that bot (with what only it may use):
            // only that bot changes it here; the user can in Settings.
            let mine = ctx.host.session_bot(&ctx.session_id);
            let others = |j: &Job| j.bot_id.is_some() && j.bot_id != mine;
            if action == "delete" {
                let Some(id) = opt_str(&args, "id") else {
                    return ToolOutput::error("`id` is required.");
                };
                if ctx.host.jobs().iter().any(|j| j.id == id && others(j)) {
                    return ToolOutput::error(format!(
                        "Job {id} is a bot's duty: only that bot, or the user in the bot's editor, can change it."
                    ));
                }
                return match ctx.host.delete_job(id) {
                    Ok(true) => ToolOutput::text(format!("Deleted job {id}.")),
                    Ok(false) => ToolOutput::error(format!("No job {id}.")),
                    Err(e) => ToolOutput::error(e.to_string()),
                };
            }
            let mut job = match (action, opt_str(&args, "id")) {
                ("update", Some(id)) => match ctx.host.jobs().into_iter().find(|j| j.id == id) {
                    Some(j) if others(&j) => {
                        return ToolOutput::error(format!(
                            "Job {id} is a bot's duty: only that bot, or the user in the bot's editor, can change it."
                        ));
                    }
                    Some(j) => j,
                    None => return ToolOutput::error(format!("No job {id}.")),
                },
                ("update", None) => return ToolOutput::error("`id` is required."),
                _ => Job {
                    cwd: ctx.cwd.display().to_string(),
                    enabled: true,
                    permission: PermissionMode::Auto,
                    ..Default::default()
                },
            };
            if let Some(v) = opt_str(&args, "name") {
                job.name = v.to_string();
            }
            if let Some(v) = opt_str(&args, "schedule") {
                job.schedule = v.to_string();
            }
            if let Some(v) = opt_str(&args, "prompt") {
                job.prompt = v.to_string();
            }
            if let Some(v) = opt_str(&args, "until") {
                // "never" (or nothing) takes the end date away.
                job.until = match v.trim() {
                    "" | "never" | "none" => Some(0),
                    v => match crate::jobs::parse_until(v, crate::util::now_ms()) {
                        Ok(t) => Some(t),
                        Err(e) => {
                            return ToolOutput::error(format!(
                                "`until`: {e}. Use e.g. `2026-10-20`, `2026-10-20 18:00` or `in 3d`."
                            ));
                        }
                    },
                };
            }
            if let Some(v) = args.get("max_runs").and_then(Value::as_u64) {
                // 0 takes the limit away.
                job.max_runs = Some(v.min(u32::MAX as u64) as u32);
            }
            if let Some(v) = opt_bool(&args, "enabled") {
                job.enabled = v;
            }
            if let Some(v) = opt_bool(&args, "unattended") {
                job.permission = if v {
                    PermissionMode::Auto
                } else {
                    PermissionMode::Default
                };
            }
            if job.prompt.is_empty() || job.schedule.is_empty() {
                return ToolOutput::error("`schedule` and `prompt` are required.");
            }
            job.permission =
                super::cap_permission(job.permission, ctx.host.permission(&ctx.session_id));

            if job.name.is_empty() {
                job.name = crate::util::one_line(&job.prompt, 40);
            }
            // A bot scheduling work makes it one of its own duties.
            if job.bot_id.is_none() {
                job.bot_id = ctx.host.session_bot(&ctx.session_id);
            }
            match ctx.host.save_job(job) {
                Ok(j) => {
                    let mut text = format!(
                        "Saved job {} “{}” (`{}`), next run {}.",
                        j.id,
                        j.name,
                        j.schedule,
                        j.next_run
                            .map(crate::jobs::fmt_time)
                            .unwrap_or_else(|| "—".into())
                    );
                    match (j.max_runs, j.until) {
                        (None, None) => text.push_str(" It runs until it is changed or deleted."),
                        (Some(m), _) => text.push_str(&format!(" It ends after {m} run(s).")),
                        (None, Some(u)) => {
                            text.push_str(&format!(" It ends {}.", crate::jobs::fmt_time(u)))
                        }
                    }
                    ToolOutput::text(text)
                }
                Err(e) => ToolOutput::error(format!("Not saved: {e}")),
            }
        })
    }
}

pub struct MessageBot;

/// Why handing work to `bot` reaches further than this session can: more
/// permissions, saved passwords or machine access of its own.
fn handoff_reasons(bot: &str, ctx: &ToolCtx) -> Vec<String> {
    let mine = ctx.host.permission(&ctx.session_id);
    let mut why = Vec::new();
    if let Some(theirs) = ctx.host.bot_permission(bot)
        && super::permission_rank(theirs) > super::permission_rank(mine)
    {
        why.push(format!(
            "{bot} works with {} permissions, more than this session's {}.",
            super::permission_name(theirs),
            super::permission_name(mine)
        ));
    }
    // Passwords and keys only that bot may use: asking it is a way to use
    // them from here.
    if ctx.host.bot_has_own_secrets(&ctx.session_id, bot) {
        why.push(format!(
            "{bot} can use saved passwords or keys this chat can't."
        ));
    }
    if let Some(machine) = ctx.host.bot_more_machines(&ctx.session_id, bot) {
        why.push(format!("{bot} may do more on {machine} than this chat."));
    }
    why
}

impl Tool for MessageBot {
    fn name(&self) -> &'static str {
        "message_bot"
    }
    fn description(&self) -> String {
        "Send a private message to another bot. It works with its own tools, permissions and memory, so hand it work that \
         belongs to it (e.g. ask the dev bot to fix code, or the assistant to schedule something). Write a complete, \
         self-contained request. It returns at once: the bot's reply arrives in this chat as a message and you continue \
         then, so keep working meanwhile and don't poll. `wait: true` blocks until it replies; only if you need the reply \
         to go on in this turn."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "bot": {"type": "string", "description": "Handle of the bot, e.g. \"@forge\"."},
                "message": {"type": "string"},
                "wait": {"type": "boolean"}
            },
            "required": ["bot", "message"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    /// Asking a bot that may do more than this session is a way around this
    /// session's approvals. (With "Ask me first" for chats, `run` asks.)
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let Some(bot) = opt_str(args, "bot") else {
            return Risk::None;
        };
        let why = handoff_reasons(bot, ctx);
        if why.is_empty() || ctx.host.bot_handoff(&ctx.session_id) == BotHandoff::Ask {
            return Risk::None;
        }
        Risk::Ask {
            reason: why.join(" "),
            key: format!("message:{}", bot.trim_start_matches('@').to_lowercase()),
        }
    }
    fn summary(&self, args: &Value) -> String {
        format!(
            "{} · {}",
            opt_str(args, "bot").unwrap_or("?"),
            crate::util::one_line(opt_str(args, "message").unwrap_or(""), 70)
        )
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let bot = match arg_str(&args, "bot") {
                Ok(b) => b.to_string(),
                Err(e) => return e,
            };
            let message = match arg_str(&args, "message") {
                Ok(m) => m.to_string(),
                Err(e) => return e,
            };
            let ask = |reason: String| {
                ctx.host.approve_reason(super::Approve {
                    session_id: ctx.session_id.clone(),
                    call_id: ctx.call_id.clone(),
                    tool: "message_bot".into(),
                    summary: format!("Hand work to {bot}\n\n{message}"),
                    reason,
                    key: format!("message:{}", bot.trim_start_matches('@').to_lowercase()),
                    always: true,
                    cancel: ctx.cancel.clone(),
                    arguments: json!({ "bot": bot, "message": message, "wait": opt_bool(&args, "wait").unwrap_or(false) }),
                    once: ctx.host.bot_handoff(&ctx.session_id) == BotHandoff::Ask,
                })
            };
            match ctx.host.bot_handoff(&ctx.session_id) {
                // Refused by the engine too; said here before anyone waits.
                BotHandoff::Off => {
                    return ToolOutput::error(
                        "Chats may not hand work to bots: the user turned that off (Settings → Privacy & safety). Do it here, or ask the user to talk to the bot.",
                    );
                }
                // The user wants to see each hand-off, whatever the mode.
                BotHandoff::Ask => {
                    let mut reason = format!(
                        "This chat wants {bot} to do something for it. You asked to approve each time a chat hands work to a bot."
                    );
                    for r in handoff_reasons(&bot, ctx) {
                        reason.push(' ');
                        reason.push_str(&r);
                    }
                    if let Err(why) = ask(reason).await {
                        return ToolOutput::error(why);
                    }
                }
                // A bot the user limited on a machine handing work to one
                // that may do more there: asked even in YOLO, where `assess`
                // isn't (the user set that limit on purpose).
                BotHandoff::On => {
                    if ctx.host.permission(&ctx.session_id) == PermissionMode::Auto
                        && let Some(machine) = ctx.host.bot_more_machines(&ctx.session_id, &bot)
                        && let Err(why) = ask(format!(
                            "{bot} may do more on {machine} than this chat, which you limited there."
                        ))
                        .await
                    {
                        return ToolOutput::error(why);
                    }
                }
            }
            // In the background unless asked to wait (or where only waiting
            // works: subagents, jobs, rooms, bots answering bots).
            let wait = opt_bool(&args, "wait").unwrap_or(false)
                || ctx.depth > 0
                || !ctx.host.may_hand_off_in_background(&ctx.session_id);
            if !wait {
                return match ctx
                    .host
                    .hand_to_bot(&ctx.session_id, &bot, &message, &ctx.call_id)
                {
                    Ok(h) => ToolOutput::text(format!(
                        "Sent to {0}, which is working on it now in its own conversation. Its reply will arrive in this chat \
                         as a message from {0}, and you will continue then. Meanwhile go on with other work; don't wait, poll \
                         or send it again. If nothing else can be done until it replies, tell the user you are waiting for {0} \
                         and end your turn.",
                        h.bot
                    ))
                    .with_details(json!({
                        "bot": h.bot,
                        "session_id": h.session_id,
                        "background": true,
                    })),
                    Err(e) => ToolOutput::error(e),
                };
            }
            (ctx.progress)(&format!("Waiting for {bot}…\n"));
            match ctx
                .host
                .message_bot(&ctx.session_id, &bot, &message, ctx.cancel.clone())
                .await
            {
                Ok(reply) => {
                    let (text, _) = super::fit_output(&reply, ctx, "bot reply");
                    ToolOutput::text(text)
                }
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    fn ledger(status: PlanStatus) -> TaskLedger {
        TaskLedger {
            plan: vec![PlanItem {
                text: "Check both files".into(),
                status,
            }],
            done_criteria: vec!["Tests pass".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn durable_tools_classify_each_action_and_refuse_direct_read_only_calls() {
        // Engine installs a process-wide child reaper. Isolate it so the
        // ordinary file and process tests retain ownership of their children.
        const CHILD: &str = "TESSERA_TEST_DURABLE_PERMISSIONS";
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::meta::completion_tests::durable_tools_classify_each_action_and_refuse_direct_read_only_calls", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let (ctx, _dir) = crate::engine::test_support::read_only_ctx().await;
        type Actions = (Box<dyn Tool>, Vec<Value>, Vec<Value>);
        let cases: Vec<Actions> = vec![
            (
                Box::new(crate::tools::note::Note),
                vec![json!({"action":"list"}), json!({"action":"read"})],
                ["create", "edit", "update", "image"]
                    .map(|a| json!({"action":a}))
                    .into(),
            ),
            (
                Box::new(MemoryTool),
                vec![
                    json!({"action":"list"}),
                    json!({"action":"add", "preview":true}),
                ],
                ["add", "replace", "remove"]
                    .map(|a| json!({"action":a,"target":"user"}))
                    .into(),
            ),
            (
                Box::new(SkillManage),
                vec![],
                ["create", "update", "write_file", "delete"]
                    .map(|a| json!({"action":a}))
                    .into(),
            ),
            (
                Box::new(Schedule),
                vec![json!({"action":"list"})],
                ["create", "update", "delete", "end"]
                    .map(|a| json!({"action":a}))
                    .into(),
            ),
        ];
        for (tool, reads, writes) in cases {
            for args in reads {
                assert_eq!(tool.action_kind(&args), ToolKind::Read);
                assert!(!matches!(
                    super::super::permission_risk(
                        tool.as_ref(),
                        &args,
                        &ctx,
                        PermissionMode::ReadOnly
                    ),
                    Risk::Block { .. }
                ));
            }
            for args in writes {
                assert_eq!(tool.action_kind(&args), ToolKind::Write);
                assert!(matches!(
                    super::super::permission_risk(
                        tool.as_ref(),
                        &args,
                        &ctx,
                        PermissionMode::ReadOnly
                    ),
                    Risk::Block { .. }
                ));
                assert!(matches!(
                    super::super::permission_risk(tool.as_ref(), &args, &ctx, PermissionMode::Ask),
                    Risk::Ask { .. }
                ));
                let out = tool.run(args, &ctx).await;
                assert!(
                    out.is_error && tessera_proto::plain_text(&out.content).contains("read-only"),
                    "{out:?}"
                );
            }
        }
        assert_eq!(
            super::super::permission_risk(
                &Task,
                &json!({"goal":"Fixture"}),
                &ctx,
                PermissionMode::ReadOnly
            ),
            Risk::None
        );
    }

    #[test]
    fn blocked_plan_does_not_establish_criteria_dependencies() {
        let mut l = ledger(PlanStatus::Blocked);
        assert!(completion_note(&l).unwrap().contains("Tests pass"));
        l.criteria.push(CriterionState {
            status: CriterionStatus::Unmet,
            evidence: "Can fix tests now".into(),
        });
        assert!(completion_note(&l).unwrap().contains("[unmet]"));
        l.criteria[0].status = CriterionStatus::Unverifiable;
        assert!(completion_note(&l).is_none());
        l.plan[0].status = PlanStatus::InProgress;
        assert!(completion_note(&l).is_some());
        assert!(completion_note(&TaskLedger::default()).is_none());
    }
    #[test]
    fn criteria_alone_and_pending_work_keep_the_early_finish_guard() {
        let mut l = ledger(PlanStatus::Pending);
        assert!(completion_note(&l).is_some());
        l.plan.clear();
        assert!(completion_note(&l).is_some());
        l.criteria = vec![CriterionState {
            status: CriterionStatus::Unverifiable,
            evidence: "No device".into(),
        }];
        assert!(completion_note(&l).is_none());
    }

    #[test]
    fn open_state_tracks_statuses_but_not_notes_or_evidence_wording() {
        let mut l = ledger(PlanStatus::Pending);
        let before = completion_state(&l);
        l.notes.push("A new note".into());
        l.criteria.push(CriterionState {
            status: CriterionStatus::Unchecked,
            evidence: "Still looking".into(),
        });
        l.plan.insert(
            0,
            PlanItem {
                text: "Closed work".into(),
                status: PlanStatus::Done,
            },
        );
        assert_eq!(completion_state(&l), before);
        l.criteria[0].status = CriterionStatus::Unmet;
        assert_ne!(completion_state(&l), before);
        l.plan[1].status = PlanStatus::InProgress;
        assert!(completion_note(&l).unwrap().contains("in progress"));
    }

    #[test]
    fn acknowledgements_are_narrow_and_split_parts_are_detected() {
        use tessera_proto::AssistantPart;
        for t in ["✓", " ✔\n", "Done ✓", "✓ Checked.", "✅ OK!"] {
            assert!(is_check_ack(t), "{t}");
        }
        for t in [
            "",
            "Done",
            "✓ Tests pass",
            "✓ all criteria met",
            "Use ✓ for completed steps",
            "✓\nTests pass",
        ] {
            assert!(!is_check_ack(t), "{t}");
        }
        assert!(check_ack_parts(&[
            AssistantPart::Text {
                text: "Done ".into()
            },
            AssistantPart::Text { text: "✓".into() }
        ]));
        assert!(possible_check_ack("Ch"));
        assert!(!possible_check_ack("Checked the files"));
    }
}

#[cfg(test)]
mod ledger_size_tests {
    use super::*;

    #[test]
    fn caps_never_archive_live_requirements_or_unmet_criteria() {
        let mut ledger = TaskLedger {
            goal: "A current goal".into(),
            constraints: vec!["Do not loosen security. ".repeat(150)],
            plan: vec![PlanItem {
                text: "pending work ".repeat(200),
                status: PlanStatus::Pending,
            }],
            done_criteria: vec!["must be checked ".repeat(200)],
            criteria: vec![CriterionState {
                status: CriterionStatus::Unmet,
                evidence: "Not checked".into(),
            }],
            ..Default::default()
        };
        let original = ledger.clone();
        let mut archive = ArchiveItems::new();
        bound_ledger(&mut ledger, &mut archive);
        assert_eq!(ledger, original);
        assert!(archive.is_empty());
        assert!(
            validate_live_ledger(&ledger)
                .unwrap_err()
                .to_string()
                .contains("No update saved")
        );
    }

    #[test]
    fn settled_plan_and_criteria_are_archived_without_losing_status_or_evidence() {
        let mut ledger = TaskLedger {
            plan: vec![
                PlanItem {
                    text: "finished ".repeat(300),
                    status: PlanStatus::Done,
                },
                PlanItem {
                    text: "current".into(),
                    status: PlanStatus::Pending,
                },
            ],
            done_criteria: vec![
                "long finished criterion ".repeat(150),
                "current criterion".into(),
            ],
            criteria: vec![
                CriterionState {
                    status: CriterionStatus::Met,
                    evidence: "passed with exact evidence".into(),
                },
                CriterionState {
                    status: CriterionStatus::Unmet,
                    evidence: "not yet checked".into(),
                },
            ],
            ..Default::default()
        };
        let mut archive = ArchiveItems::new();
        bound_ledger(&mut ledger, &mut archive);
        validate_live_ledger(&ledger).unwrap();
        assert_eq!(ledger.plan[0].text, "current");
        assert_eq!(ledger.criteria[0].status, CriterionStatus::Unmet);
        assert!(archive["plan"][0].contains("finished "));
        assert!(archive["done_criteria"][0].contains("passed with exact evidence"));
    }
}
