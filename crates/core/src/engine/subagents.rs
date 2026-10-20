//! Subagents: agent definitions, background swarms, structured reports,
//! worktree isolation, and continuing a finished subagent.
//!
//! A subagent is a session of kind `subagent` under its parent. How it was
//! started (definition, report format, isolation) is kept with its session
//! (`SessionInternal::subagent`), so it can be continued the same way, even
//! after a restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::agents::{self, AgentDef, SubagentSpec};
use crate::tools::file_object::DirectoryScope;
use crate::tools::meta::{Started, SubAction};
use crate::worktree::{self, WorktreeState, WorktreeStatus};

/// Background subagents one conversation, and the whole server, may have
/// started and not finished (working or waiting their turn: see
/// `engine/helper_limits.rs`).
const MAX_BACKGROUND_PER_SESSION: usize = 64;
const MAX_BACKGROUND_TOTAL: usize = 256;

/// Provisional roster entries count under the same lock as real helpers.
/// Creation/validation failure drops only this batch's unclaimed entries.
struct RosterReservation {
    inner: Arc<Inner>,
    parent: String,
    ids: Vec<String>,
}
impl Drop for RosterReservation {
    fn drop(&mut self) {
        let mut bg = self.inner.background.lock();
        if let Some(list) = bg.get_mut(&self.parent) {
            list.retain(|a| !self.ids.contains(&a.session_id));
            if list.is_empty() {
                bg.remove(&self.parent);
            }
        }
    }
}
/// Clear run-local call identities only after all report/format retries.
/// Planned restart leaves accounting intact for automatic continuation.
struct HelperBudgetFinish {
    inner: Arc<Inner>,
    rt: Arc<Rt>,
}
impl Drop for HelperBudgetFinish {
    fn drop(&mut self) {
        self.inner.complete_helper_budget(&self.rt);
    }
}
/// A task, checked and resolved before any subagent of its batch starts.
struct Plan {
    task: SubTask,
    cwd: PathBuf,
    model: Option<ModelRef>,
    readonly: bool,
    spec: SubagentSpec,
    note: Option<String>,
}

/// What one run of a subagent produced.
pub(super) struct ChildOutcome {
    result: Result<String, String>,
    /// A line for the parent beside the report (worktree changes).
    note: Option<String>,
    /// Immutable authority of this helper run, before report/worktree delays.
    stops: StopMarks,
}

impl ChildOutcome {
    fn err(e: impl Into<String>, stops: StopMarks) -> Self {
        Self {
            result: Err(e.into()),
            note: None,
            stops,
        }
    }
}

fn join_notes(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
        (a, b) => a.or(b),
    }
}

impl Inner {
    /// Agent definitions available to a session working in `cwd`.
    pub(super) fn agent_defs(&self, cwd: &Path) -> Vec<AgentDef> {
        agents::discover(cwd, &self.paths.agents_dir(), &self.protected_paths())
    }

    /// The `agent` tool's description for this session: fixed at its first
    /// request so the tool list stays byte-stable (prompt caching).
    pub(super) fn agent_description(&self, st: &RtState) -> String {
        st.agent_list
            .get_or_init(|| {
                crate::tools::meta::Agent::describe(&self.agent_defs(Path::new(&st.info.cwd)))
            })
            .clone()
    }

    /// Choosing a helper's cwd must not turn an out-of-scope folder into
    /// its implicitly writable working directory. Machine permissions are
    /// inherited separately; this cwd always names a folder on the server.
    fn helper_folder(&self, parent: &RtState, raw: &str) -> Result<(PathBuf, bool), String> {
        let base = crate::tools::canonical(Path::new(&parent.info.cwd));
        let path = crate::util::expand_home(raw);
        let path = if path.is_absolute() {
            path
        } else {
            base.join(path)
        };
        if crate::tools::claude_state_path(&path) {
            return Err("Claude Code owns its account files; helpers cannot work there.".into());
        }
        let path = crate::platform::canonicalize(&path)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| format!("helper cwd `{raw}` is not an existing folder"))?;
        self.helper_path(parent, &path)
            .map(|readonly| (path, readonly))
    }

    /// Check existing or not-yet-created paths against the parent's scope.
    /// In particular, a checkout is not authorized by becoming a helper cwd.
    fn helper_path(&self, parent: &RtState, path: &Path) -> Result<bool, String> {
        let base = crate::tools::canonical(Path::new(&parent.info.cwd));
        let path = crate::tools::canonical(path);
        let protected = self.protected_paths().canonical();
        if crate::tools::claude_state_path(&path)
            || protected.covers(&path)
            || self.other_chats_upload(&parent.info.id, &path)
        {
            return Err(
                "helper cwd is protected; choose a project folder this chat may already use".into(),
            );
        }
        let access = self.write_access();
        let rule = access.rule(&path);
        if !path.starts_with(base) && rule.is_none() {
            return Err(format!(
                "helper cwd/worktree path {} is outside this chat's folder scope; choose a folder inside {} or an allowed folder",
                path.display(),
                parent.info.cwd
            ));
        }
        Ok(rule == Some(false))
    }

    /// Resolve every task (definition, model, isolation) before starting
    /// any, so one bad task can't leave half a batch running.
    fn plan_tasks(&self, parent_rt: &Rt, tasks: Vec<SubTask>) -> Result<Vec<Plan>, String> {
        let (cwd, claude) = {
            let st = parent_rt.st.lock();
            if !self.helpers_enabled(&st) {
                return Err(
                    "Helpers cannot start helpers or are switched off for this chat.".into(),
                );
            }
            (PathBuf::from(&st.info.cwd), self.runs_claude_code(&st))
        };
        let available = self
            .providers
            .infos()
            .into_iter()
            .filter(|p| p.authenticated)
            .flat_map(|p| p.models)
            .map(|m| ModelRef {
                provider: m.provider,
                model: m.id,
            })
            .collect::<Vec<_>>();
        let resolve = |raw: &str| {
            let model = self.resolve_model(raw)?;
            if claude && !available.contains(&model) {
                return Err(format!(
                    "unknown model `{raw}`. Available: {}",
                    available
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(model)
        };

        let nonempty = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let mut plans = Vec::new();
        for task in tasks {
            let (task_cwd, folder_readonly) = self
                .helper_folder(
                    &parent_rt.st.lock(),
                    task.cwd.as_deref().unwrap_or(&cwd.display().to_string()),
                )
                .map_err(|e| format!("{}: {e}", task.name))?;
            let cwd_scope = DirectoryScope::capture(&task_cwd)?;
            let defs = if task.agent.is_some() {
                self.agent_defs(&task_cwd)
            } else {
                Vec::new()
            };
            let def = match nonempty(&task.agent) {
                None => None,
                Some(a) => Some(
                    defs.iter()
                        .find(|d| d.name.eq_ignore_ascii_case(&a))
                        .cloned()
                        .ok_or_else(|| {
                            let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                            format!(
                                "{}: there is no agent type `{a}`; available: {}",
                                task.name,
                                names.join(", ")
                            )
                        })?,
                ),
            };
            let mut note = None;
            let model = match (
                nonempty(&task.model),
                def.as_ref().and_then(|d| d.model.clone()),
            ) {
                (Some(m), _) => Some(resolve(&m).map_err(|e| format!("{}: {e}", task.name))?),
                // A definition travels between machines: if its model isn't
                // here, the task still runs, on this chat's model.
                (None, Some(m)) if claude => {
                    Some(resolve(&m).map_err(|e| format!("{}: {e}", task.name))?)
                }
                (None, Some(m)) => match self.resolve_model(&m) {
                    Ok(r) => Some(r),
                    Err(_) => {
                        note = Some(format!(
                            "model `{m}` of agent type `{}` is not available here; it runs on this chat's model",
                            def.as_ref().map(|d| d.name.as_str()).unwrap_or("?")
                        ));
                        None
                    }
                },
                _ if claude => {
                    let available = available
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let m = self.default_model().ok_or_else(|| format!(
                        "{}: no default helper model; set `model` on the task. Available: {available}", task.name
                    ))?;
                    if self.providers.kind_of(&m.provider) == Some(ProviderKind::ClaudeCode) {
                        return Err(format!(
                            "{}: the server default is Claude Code; set `model` on the task (provider/model) to choose deliberately. Available: {available}",
                            task.name
                        ));
                    }
                    Some(resolve(&m.to_string())?)
                }
                _ => None,
            };
            // Daybreak Blue is OpenAI's: the helper's own model must offer it.
            if task.daybreak == Some(true) {
                let target = model
                    .clone()
                    .unwrap_or_else(|| parent_rt.st.lock().info.model.clone());
                if !self.providers.daybreak_available(&target) {
                    return Err(format!(
                        "{}: Daybreak Blue needs an OpenAI model your account offers it on; set `model` (e.g. openai-codex/gpt-6.1-sol)",
                        task.name
                    ));
                }
            }
            let readonly = task.readonly
                || folder_readonly
                || self.effective_permission(&parent_rt.st.lock()) == PermissionMode::ReadOnly
                || def.as_ref().is_some_and(|d| d.readonly);
            let worktree = if task.worktree && !readonly {
                let top = worktree::find_top(&task_cwd).ok_or_else(|| {
                    format!(
                        "{}: isolation \"worktree\" needs a git repository; {} is not in one. Set this task's cwd to a repository folder within the chat folder.",
                        task.name,
                        task_cwd.display()
                    )
                })?;
                let (_, repo_readonly) =
                    self.helper_folder(&parent_rt.st.lock(), &top.display().to_string())?;
                if repo_readonly {
                    return Err(format!(
                        "{}: the repository is read-only; worktree isolation cannot widen that permission",
                        task.name
                    ));
                }
                Some(WorktreeState {
                    subdir: task_cwd
                        .strip_prefix(&top)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    repo: top.display().to_string(),
                    repo_scope: Some(DirectoryScope::capture(&top)?),
                    ..Default::default()
                })
            } else {
                None
            };
            let (max_steps, max_minutes) = def
                .as_ref()
                .map(|d| {
                    d.budget(
                        super::helper_limits::UNLIMITED,
                        super::helper_limits::UNLIMITED,
                    )
                })
                .unwrap_or((None, None));
            // A project's definition is as trustworthy as its AGENTS.md:
            // hidden text is stripped and instruction-shaped text labelled.
            let role = def
                .as_ref()
                .filter(|d| !d.prompt.trim().is_empty())
                .map(|d| {
                    if d.source != agents::Source::Project {
                        return d.prompt.clone();
                    }
                    let scan = crate::scan::scan(&d.prompt);
                    if scan.is_clean() {
                        scan.text
                    } else {
                        format!(
                            "{}\n{}",
                            crate::scan::label("agent definition", &scan),
                            scan.text
                        )
                    }
                });
            let spec = SubagentSpec {
                name: task.name.clone(),
                agent: def.as_ref().map(|d| d.name.clone()),
                role,
                tools: def.and_then(|d| d.tools),
                max_steps,
                max_minutes,
                output: task.output.clone(),
                worktree,
                cwd_scope: Some(cwd_scope),
                queued: true,
                interrupted_by_restart: false,
            };
            plans.push(Plan {
                task,
                cwd: task_cwd,
                model,
                readonly,
                spec,
                note,
            });
        }
        Ok(plans)
    }

    /// Create a subagent's session under `parent_rt`, primed with its task.
    fn start_child(&self, parent_rt: &Rt, call_id: &str, plan: &Plan) -> Result<Arc<Rt>, String> {
        self.verify_source_scope(&plan.spec)?;
        // Its task is the starting agent's words (its bot, or the chat), not the user's.

        let author = super::agent_author(&parent_rt.st.lock());
        // A helper belongs to its parent's turn: one from before a Stop
        // runs no tool after it either.
        let parent_stops = parent_rt.st.lock().turn_stops.clone();
        let rt = {
            let pst = parent_rt.st.lock();
            // Never more than the parent itself may do (a bot's own default
            // can be higher than the session it's in).
            let perm = Some(if plan.readonly {
                PermissionMode::ReadOnly
            } else {
                self.effective_permission(&pst)
            });
            let bot = self.speaker_bot(&pst);
            self.create_session(
                Some(plan.cwd.display().to_string()),
                plan.model.clone(),
                Some(format!("↳ {}", plan.task.name)),
                SessionKind::Subagent,
                Some(&pst),
                perm,
                bot.as_ref(),
            )
            .map_err(|e| e.to_string())?
        };
        // Its budget starts when it gets a slot (see `finish_child`), not
        // while it waits behind the rest of a swarm.
        {
            let mut st = rt.st.lock();
            st.info.cwd = plan.cwd.display().to_string();
            st.running = true;
            st.next_turn_stops = Some(parent_stops);
            st.info.status = SessionStatus::Running;
            st.info.role = plan.spec.agent.clone();
            st.internal.subagent = Some(plan.spec.clone());
            st.info.parent_call = Some(call_id.to_string());
            st.info.helper_owner = Some(HelperOwner::Tessera);
            if let Some(on) = plan.task.daybreak {
                st.info.daybreak = on && self.providers.daybreak_available(&st.info.model);
            }
        }
        self.save(&rt);
        let mut prompt = plan.task.prompt.clone();
        if plan.spec.worktree.is_some() {
            prompt.push_str(
                "\n\n(You work in a private git worktree of this repository: edit files as usual; your changes go back to \
                 the agent that started you when you finish. Ignored files such as dependencies and build outputs are not \
                 in it. Don't commit, push or switch branches.)",
            );
        }
        if let Some(fmt) = &plan.spec.output {
            prompt.push_str("\n\n");
            prompt.push_str(&agents::output_instruction(fmt));
        }
        self.append(
            &rt,
            EntryBody::User {
                content: vec![ContentPart::text(prompt)],
                author: Some(author),
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(rt)
    }

    /// Never recapture an old helper's pathname as new authority. Older
    /// records without identity must be started again by their parent.
    fn verify_source_scope(&self, spec: &SubagentSpec) -> Result<(), String> {
        spec.cwd_scope
            .as_ref()
            .ok_or("This helper has no bound working-folder identity; start a new helper.")?
            .verify()
    }

    /// A Tessera helper's persisted folder identities, copied from its state
    /// without touching the filesystem: callers check them after releasing
    /// the state lock, so a stalled network folder can't hold up the chat.
    pub(super) fn helper_identities(&self, st: &RtState) -> Result<Option<HelperFolder>, String> {
        if st.info.helper_owner != Some(HelperOwner::Tessera)
            || st.info.kind != SessionKind::Subagent
        {
            return Ok(None);
        }
        let spec = st
            .internal
            .subagent
            .as_ref()
            .ok_or("Helper working-folder authority is missing.")?;
        let source = spec
            .cwd_scope
            .clone()
            .ok_or("This helper has no bound working-folder identity; start a new helper.")?;
        let active = match spec.worktree.as_ref() {
            Some(w) => w
                .cwd_scope
                .clone()
                .ok_or("The helper worktree has no active working-folder authority.")?,
            None => source.clone(),
        };
        if active.path() != Path::new(&st.info.cwd) {
            return Err("The helper's working folder no longer matches its authorization; start a new helper.".into());
        }
        Ok(Some(HelperFolder { source, active }))
    }

    /// The helper's checked working folder (`None` for other chats). The
    /// state lock is held only to copy the identities.
    pub(super) fn helper_authorized_cwd(&self, rt: &Rt) -> Result<Option<PathBuf>, String> {
        let folder = self.helper_identities(&rt.st.lock())?;
        folder
            .map(|f| f.verified().map(|s| s.path().to_path_buf()))
            .transpose()
    }

    pub(super) fn helper_authorized_scope(
        &self,
        rt: &Rt,
    ) -> Result<Option<DirectoryScope>, String> {
        let folder = self.helper_identities(&rt.st.lock())?;
        folder.map(|f| f.verified().cloned()).transpose()
    }

    /// The folder a command of `rt` starts in, held open until it has
    /// started: a helper's own checked folder, or a folder below it reached
    /// from that held folder; other chats' `cwd` as it is now. The state
    /// lock is held only to copy the helper's identities.
    pub(super) fn process_directory(
        &self,
        rt: &Rt,
        cwd: &Path,
    ) -> Result<crate::tools::file_object::ProcessDirectory, String> {
        let folder = self.helper_identities(&rt.st.lock())?;
        let Some(folder) = folder else {
            return crate::tools::file_object::ProcessDirectory::open(cwd);
        };
        if folder.active != folder.source {
            folder.source.verify()?;
        }
        let root = folder.active.open()?;
        if cwd == folder.active.path() {
            return Ok(root);
        }
        let selected = crate::platform::canonicalize(cwd).map_err(|e| e.to_string())?;
        if !cwd.starts_with(folder.active.path()) || !selected.starts_with(folder.active.path()) {
            return Err("The helper command folder must remain inside its bound working folder; select another helper cwd explicitly.".into());
        }
        root.open_beneath(&selected)
    }

    /// The tools a definition allows (`None`: all the session may have).
    fn child_tools(&self, spec: &SubagentSpec) -> Option<Vec<&'static str>> {
        let allow = spec.tools.as_ref()?;
        let reg = self.registry.read();
        Some(
            reg.available(PermissionMode::Auto, 1, true)
                .filter(|t| agents::tool_allowed(allow, t.name()))
                .map(|t| t.name())
                .collect(),
        )
    }

    /// Report the actual role, permission ceiling and tools, not the role's
    /// nominal allowlist. This never grants a missing capability.
    fn child_capabilities(&self, rt: &Rt, prompt: Option<&str>) -> String {
        // Before the state lock: the sandbox looks up the root chat.
        let sandboxed = {
            let id = rt.st.lock().info.id.clone();
            self.session_sandbox(&id).is_some() && crate::sandbox::confines_writes()
        };
        let st = rt.st.lock();
        let mode = self.effective_permission(&st);
        let tools: Vec<String> = self
            .base_tool_specs(&st)
            .into_iter()
            .map(|t| t.name)
            .collect();
        let role = st
            .internal
            .subagent
            .as_ref()
            .and_then(|s| s.agent.as_deref())
            .unwrap_or("general");
        let mut out = format!(
            "role: {role}; permission: {mode:?}; effective tools: {}",
            tools.join(", ")
        );
        if mode == PermissionMode::ReadOnly {
            out.push_str(&format!(
                ". Read-only removes bash/job and file writes; {}; machines, if listed, permits only list/read/ls",
                crate::tools::bash::read_only_commands(sandboxed)
            ));
        }
        let id = st.info.id.clone();
        out.push_str(&format!("; cwd: {}", st.info.cwd));
        if let Some(w) = st
            .internal
            .subagent
            .as_ref()
            .and_then(|s| s.worktree.as_ref())
        {
            if !w.path.is_empty() {
                out.push_str(&format!("; worktree: {} ({:?})", w.path, w.status));
            } else if Path::new(&w.repo).join(".git").is_dir() {
                let planned = Path::new(&w.repo).join(".git/tessera/worktrees").join(&id);
                out.push_str(&format!(
                    "; planned worktree: {} (created after slot; verify status)",
                    planned.display()
                ));
            } else {
                out.push_str(&format!("; worktree repository: {} (linked checkout path assigned after slot; use status)", w.repo));
            }
        }
        drop(st);
        if tools.iter().any(|t| t == "machines") {
            let machines: Vec<String> = self
                .machine_list()
                .into_iter()
                .filter_map(|m| {
                    self.machine_access(&id, &m.id, &m.scope)
                        .ok()
                        .map(|access| {
                            let reads_only = mode == PermissionMode::ReadOnly
                                || access == MachineAccess::ReadOnly;
                            format!(
                                "{} ({}, {}; {})",
                                m.name,
                                m.id,
                                if m.online { "online" } else { "offline" },
                                if reads_only {
                                    "list/read/ls only"
                                } else {
                                    "machine's own scope applies; use machines list"
                                }
                            )
                        })
                })
                .collect();
            if machines.is_empty()
                && prompt.is_some_and(|p| p.to_ascii_lowercase().contains("machine"))
            {
                out.push_str("\nWarning: no authorized machine is available to this helper; supply local evidence to review.");
            }
            out.push_str(&format!(
                "; authorized machines: {}",
                if machines.is_empty() {
                    "none".to_string()
                } else {
                    machines.join(", ")
                }
            ));
        }
        if let Some(prompt) = prompt {
            for warning in capability_warnings(prompt, &tools, mode == PermissionMode::ReadOnly) {
                out.push_str(&format!("\nWarning: {warning}"));
            }
        }
        out
    }
    /// Run a subagent to the end (or its hard deadline) and extract its
    /// answer: checked against the requested format, with the changes of
    /// an isolated one collected.
    pub(super) async fn finish_child(
        self: Arc<Self>,
        rt: Arc<Rt>,
        cancel: CancellationToken,
    ) -> ChildOutcome {
        // A read-only helper's scratch folder (build output) goes with it.
        let _scratch = crate::tools::bash::ScratchGuard(rt.st.lock().info.id.clone());
        let _finish = HelperBudgetFinish {
            inner: self.clone(),
            rt: rt.clone(),
        };
        // Stop works even before this helper gets a lane or a global slot.
        let attempt_id = self.begin_attempt(&rt).expect("helper attempt");
        let live = self.stops_here();
        let stops = {
            let mut st = rt.st.lock();
            st.cancel = Some(cancel.clone());
            st.turn_stops = st.next_turn_stops.clone().unwrap_or(live);
            st.turn_stops.clone()
        };
        // On a server that answers one request at a time, subagents take
        // turns: side by side, each one's requests would wait behind the
        // others' and push their cached prompts out, every step reading a
        // whole context again. Their clocks start once it's their turn.
        let model = self.effective_model(&rt.st.lock());
        // Capture before lane/slot waits, not after authority could change.
        let worktree_admission = self
            .action_admission(&rt.st.lock())
            .with_cancel(cancel.clone());
        let spec = rt.st.lock().internal.subagent.clone().unwrap_or_default();
        let writable_setup = spec.worktree.is_some();
        // Its chat's limit, its model's lane, the server's room.
        let turn = self
            .helper_turn(&rt, &model, &cancel, &worktree_admission, writable_setup)
            .await;
        let _turn = match turn {
            Ok(t) => t,
            Err(e) => {
                {
                    // Its start is dropped, the turn it belonged to with it.
                    let mut st = rt.st.lock();
                    st.running = false;
                    st.next_turn_stops = None;
                }
                self.set_status(&rt, SessionStatus::Interrupted);
                self.notice(&rt, NoticeLevel::Info, &e);
                self.set_attempt_outcome(
                    &rt,
                    &attempt_id,
                    crate::store::AttemptOutcome::Stopped,
                    None,
                );
                return ChildOutcome::err(
                    super::attempts::notes(Some(crate::store::AttemptOutcome::Stopped), None),
                    stops,
                );
            }
        };
        let spec = rt.st.lock().internal.subagent.clone().unwrap_or_default();
        let folder_check = self.verify_source_scope(&spec).and_then(|()| {
            if spec.worktree.is_none() {
                self.helper_authorized_cwd(&rt).map(|_| ())
            } else {
                Ok(())
            }
        });
        if let Err(e) = folder_check {
            rt.st.lock().running = false;
            self.notice(&rt, NoticeLevel::Error, &e);
            self.set_status(&rt, SessionStatus::Error);
            self.set_attempt_outcome(
                &rt,
                &attempt_id,
                crate::store::AttemptOutcome::Failed,
                Some(
                    if e.contains("folder changed") {
                        "the working folder changed before the helper could start."
                    } else {
                        "the working folder is no longer available or allowed."
                    }
                    .into(),
                ),
            );
            return ChildOutcome::err(self.child_result(&rt, &cancel, 0).0.unwrap_err(), stops);
        }
        if writable_setup && let Err(e) = self.ensure_worktree(&rt, worktree_admission).await {
            rt.st.lock().running = false;
            self.notice(&rt, NoticeLevel::Error, format!("No worktree: {e}"));
            self.set_status(&rt, SessionStatus::Error);
            self.set_attempt_outcome(
                &rt,
                &attempt_id,
                crate::store::AttemptOutcome::Failed,
                Some("the helper's worktree could not be set up safely.".into()),
            );
            return ChildOutcome::err(self.child_result(&rt, &cancel, 0).0.unwrap_err(), stops);
        }
        let (steps, minutes) = {
            let chat = rt.st.lock().info.parent_id.clone().unwrap_or_default();
            let (s, m) = self.helper_budget(&chat);
            (
                spec.max_steps.unwrap_or(s).min(s).max(1),
                spec.max_minutes.unwrap_or(m).min(m).max(1),
            )
        };
        let now = Instant::now();
        // No time limit: no deadline, nothing to stop it.
        let after = |extra: u64| {
            (minutes != super::helper_limits::UNLIMITED)
                .then(|| now.checked_add(Duration::from_secs(minutes as u64 * 60 + extra)))
                .flatten()
        };

        let tools = self.child_tools(&spec);
        let since = {
            let mut st = rt.st.lock();
            // Only explicit continuation starts a fresh run. An automatic
            // restart retains consumed units, deadline and in-flight identities.
            if spec.interrupted_by_restart && st.budget.is_some() {
                let b = st.budget.as_mut().unwrap();
                b.max_steps = b.max_steps.min(steps);
                if let Some(ceiling) = after(0) {
                    b.soft_deadline = Some(b.soft_deadline.map_or(ceiling, |d| d.min(ceiling)));
                }
            } else {
                st.budget = Some(Budget {
                    max_steps: steps,
                    base: st.info.steps,
                    used: 0,
                    calls: HashMap::new(),
                    soft_deadline: after(0),
                    wrapped: false,
                    closed: false,
                });
            }
            st.tools_only = tools;
            if let Some(spec) = st.internal.subagent.as_mut() {
                spec.queued = false;
                spec.interrupted_by_restart = false;
            }
            st.info.helper_queued = false;
            st.no_tool_calls = st.budget.as_ref().is_some_and(|b| b.wrapped);
            st.info.last_seq
        };
        let hard = rt
            .st
            .lock()
            .budget
            .as_ref()
            .and_then(|b| b.soft_deadline)
            .and_then(|d| d.checked_add(Duration::from_secs(180)));
        // It starts: no longer shown waiting.
        self.save(&rt);
        let watchdog = self.watch_attempt(rt.clone(), cancel.clone(), hard);
        self.clone().run(rt.clone(), cancel.clone()).await;
        // A slot may become free at the same instant Stop cancels it. If
        // it ended before its first step, don't leave it looking queued.
        let stopped_before_start = cancel.is_cancelled() && {
            let st = rt.st.lock();
            st.info.steps == 0 && st.info.status == SessionStatus::Idle
        };
        if stopped_before_start {
            self.set_status(&rt, SessionStatus::Interrupted);
        }
        let (mut result, mut finished) = self.child_result(&rt, &cancel, since);
        // A model can end a successful turn without reporting its work.
        // One appended message keeps the same session, tool list, budget and
        // deadline. Never restart a stopped, failed or exhausted helper.
        let empty = {
            let id = rt.st.lock().info.id.clone();
            self.store.entries(&id, since).is_ok_and(|entries| {
                !entries.last().is_some_and(|e| {
                    matches!(
                        &e.body,
                        EntryBody::Assistant {
                            stop: StopReason::Stop,
                            ..
                        }
                    ) && last_answer(std::slice::from_ref(e)).is_some()
                })
            })
        };
        let retry = !cancel.is_cancelled()
            && !self.stopping.load(std::sync::atomic::Ordering::SeqCst)
            && self.running_here_since(&stops)
            && {
                let mut st = rt.st.lock();
                let remaining = st.budget.as_ref().is_some_and(|b| {
                    !b.wrapped
                        && b.used.max(st.info.steps.saturating_sub(b.base)) < b.max_steps
                        && b.soft_deadline.is_none_or(|d| Instant::now() < d)
                });
                if empty && remaining && !st.running && st.info.status == SessionStatus::Idle {
                    st.running = true;
                    st.next_turn_stops = Some(st.turn_stops.clone());
                    true
                } else {
                    false
                }
            };
        if retry {
            if self.append(&rt, EntryBody::User {
                content: vec![ContentPart::text("You ended without a report. Finish the task, or report where you stopped, what's committed and what isn't.")],
                author: Some("harness".into()),
            }).is_ok() {
                self.clone().run(rt.clone(), cancel.clone()).await;
                (result, finished) = self.child_result(&rt, &cancel, since);
            } else {
                let mut st = rt.st.lock();
                st.running = false;
                st.next_turn_stops = None;
            }
        }
        if let (Some(fmt), Ok(_)) = (&spec.output, &result)
            && finished
            && !cancel.is_cancelled()
        {
            result = match agents::check_output(
                fmt,
                &self.latest_attempt_text(&rt).unwrap_or_default(),
            ) {
                Ok(v) => Ok(self.formatted_attempt(&rt, v.to_string())),
                Err(problem) => {
                    // One retry with no tools on offer, then the text as it is.
                    {
                        let mut st = rt.st.lock();
                        st.no_tool_calls = true;
                        st.running = true;
                        // Still the same task's turn (its Stops).
                        st.next_turn_stops = Some(st.turn_stops.clone());
                    }
                    let _ = self.append(
                        &rt,
                        EntryBody::User {
                            content: vec![ContentPart::text(agents::output_nudge(fmt, &problem))],
                            author: Some("harness".into()),
                        },
                    );
                    self.clone().run(rt.clone(), cancel.clone()).await;
                    let (retry, finished) = self.child_result(&rt, &cancel, since);
                    if !finished {
                        // Neither JSON inside stopped notes nor the earlier answer
                        // can turn an interrupted or failed retry into success.
                        retry
                    } else {
                        match retry {
                            Err(e) => Err(e),
                            Ok(text) => {
                                match agents::check_output(
                                    fmt,
                                    &self.latest_attempt_text(&rt).unwrap_or_default(),
                                ) {
                                    Ok(v) => Ok(self.formatted_attempt(&rt, v.to_string())),
                                    Err(_) => Ok(format!(
                                        "(This report is not in the requested format: {problem}. Its text follows.)\n\n{text}"
                                    )),
                                }
                            }
                        }
                    }
                }
            };
        }
        watchdog.abort();
        self.complete_helper_budget(&rt);
        let note = if spec.worktree.is_some() {
            self.collect_worktree(&rt).await
        } else {
            None
        };
        ChildOutcome {
            result,
            note,
            stops,
        }
    }

    /// A subagent's answer from what it wrote after `since`, and whether it
    /// ended with a final reply.
    fn child_result(
        &self,
        rt: &Rt,
        _cancel: &CancellationToken,
        _since: u64,
    ) -> (Result<String, String>, bool) {
        let (id, attempt) = {
            let st = rt.st.lock();
            (st.info.id.clone(), st.internal.attempts.last().cloned())
        };
        let entries = match self.store.entries(&id, 0) {
            Ok(e) => e,
            Err(e) => return (Err(e.to_string()), false),
        };
        let terminal = attempt.as_ref().and_then(|a| a.outcome);
        let finished = terminal == Some(crate::store::AttemptOutcome::Completed);
        let current: Vec<_> = entries
            .iter()
            .filter(|e| e.seq > attempt.as_ref().map_or(u64::MAX, |a| a.from_seq))
            .cloned()
            .collect();
        let mut text = super::attempts::history(&entries, attempt.as_ref());
        if !finished {
            text = join_digest(text, activity_digest(&current));
        }
        (if finished { Ok(text) } else { Err(text) }, finished)
    }

    // ---- worktree isolation ------------------------------------------------------

    /// Give an isolated subagent its private checkout: made when it gets a
    /// slot, or re-made (as it left it) to continue it.
    async fn ensure_worktree(
        &self,
        rt: &Rt,
        admission: crate::tools::admission::ActionAdmission,
    ) -> Result<(), String> {
        admission.check()?;
        worktree::admitted(admission, self.ensure_worktree_admitted(rt)).await
    }

    async fn ensure_worktree_admitted(&self, rt: &Rt) -> Result<(), String> {
        let (id, spec) = {
            let st = rt.st.lock();
            (
                st.info.id.clone(),
                st.internal.subagent.clone().unwrap_or_default(),
            )
        };
        self.verify_source_scope(&spec)?;
        let Some(mut wt) = spec.worktree.clone() else {
            return Ok(());
        };
        let repo_scope = wt
            .repo_scope
            .as_ref()
            .ok_or("The repository has no bound folder identity; start a new helper.")?;
        repo_scope.verify()?;
        let sb = self.session_sandbox(&id);
        let sb = sb.as_ref();
        let repo = worktree::repo(repo_scope.path(), sb).await?;
        let path = worktree::path_for(&repo, &id);
        self.authorize_worktree(rt, &mut wt, &repo, &path)?;
        match wt.status {
            WorktreeStatus::New => {
                worktree::register(&self.store, &path, &repo.top);
                match worktree::create(&repo, &path, sb).await {
                    Ok((base, skipped)) => {
                        if skipped > 0 {
                            self.notice(rt, NoticeLevel::Info, format!("{skipped} large untracked files were not copied into its worktree."));
                        }
                        wt.repo = repo.top.display().to_string();
                        wt.path = path.display().to_string();
                        wt.head = repo.head;
                        wt.base = base;
                    }
                    Err(e) => {
                        worktree::unregister(&self.store, &path.display().to_string());
                        return Err(e);
                    }
                }
            }
            WorktreeStatus::Discarded => {
                return Err("its changes were discarded; start a new subagent instead".into());
            }
            WorktreeStatus::Active
            | WorktreeStatus::Pending
            | WorktreeStatus::Empty
            | WorktreeStatus::Applied => {
                if path.is_dir() {
                    wt.cwd_scope
                        .as_ref()
                        .ok_or("The existing checkout has no bound folder identity.")?
                        .verify()?;
                } else {
                    let patch = match (&wt.status, &wt.patch) {
                        (WorktreeStatus::Pending | WorktreeStatus::Applied, Some(a)) => {
                            self.store.artifact(a).ok().flatten().map(|(_, c)| c)
                        }
                        _ => None,
                    };
                    worktree::register(&self.store, &path, &repo.top);
                    if let Err(e) = worktree::recreate(
                        &repo.top,
                        &path,
                        &wt.head,
                        &wt.base,
                        patch.as_deref(),
                        sb,
                    )
                    .await
                    {
                        worktree::unregister(&self.store, &wt.path);
                        return Err(e);
                    }
                }
                if wt.status == WorktreeStatus::Applied {
                    wt.base = worktree::snapshot_tree(&path, sb).await?;
                    wt.patch = None;
                }
            }
        }
        // A subdirectory in the checkout may itself be a tracked symlink.
        // Authorize it independently, then bind the exact resulting object.
        let cwd = Some(path.join(&wt.subdir))
            .filter(|p| p.is_dir())
            .unwrap_or(path);
        self.authorize_worktree_path(rt, &cwd)?;
        let cwd_scope = DirectoryScope::capture(&cwd)?;
        let cwd = cwd_scope.path().to_path_buf();
        wt.cwd_scope = Some(cwd_scope);
        wt.status = WorktreeStatus::Active;
        {
            let mut st = rt.st.lock();
            let cwd = cwd.display().to_string();
            if st.info.cwd != cwd {
                st.info.cwd = cwd;
                st.internal.system_prompt = None;
            }
            if let Some(s) = st.internal.subagent.as_mut() {
                s.worktree = Some(wt);
            }
        }
        self.save(rt);
        Ok(())
    }

    /// Resolve Git's actual metadata and checkout against the parent's scope
    /// before register/create/recreate. A .git file is not a grant of access.
    fn authorize_worktree(
        &self,
        rt: &Rt,
        wt: &mut WorktreeState,
        repo: &worktree::Repo,
        path: &Path,
    ) -> Result<(), String> {
        let original = wt
            .repo_scope
            .as_ref()
            .ok_or("The repository has no bound folder identity.")?;
        original.verify()?;
        if original.path() != repo.top || (!wt.path.is_empty() && Path::new(&wt.path) != path) {
            return Err("The repository or checkout location changed; start a new helper.".into());
        }
        self.verify_source_scope(
            rt.st
                .lock()
                .internal
                .subagent
                .as_ref()
                .ok_or("Helper authority is missing.")?,
        )?;
        for p in [&repo.top, &repo.git_dir, &repo.common, &path.to_path_buf()] {
            self.authorize_worktree_path(rt, p)?;
        }
        if crate::tools::canonical(path) != path {
            return Err(
                "The checkout destination was redirected by a link; start a new helper.".into(),
            );
        }
        if Path::new(&wt.subdir).components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            return Err(
                "The checkout subdirectory must remain inside its authorized checkout.".into(),
            );
        }
        let git_dir = DirectoryScope::capture(&repo.git_dir)?;
        if let Some(previous) = &wt.git_scope {
            previous.verify()?;
            if *previous != git_dir {
                return Err("Git's directory changed; start a new helper.".into());
            }
        }
        wt.git_scope = Some(git_dir);
        let common = DirectoryScope::capture(&repo.common)?;
        if let Some(previous) = &wt.common_scope {
            previous.verify()?;
            if *previous != common {
                return Err("Git's common directory changed; start a new helper.".into());
            }
        }
        wt.common_scope = Some(common);
        Ok(())
    }

    fn authorize_worktree_path(&self, rt: &Rt, path: &Path) -> Result<(), String> {
        let parent = rt
            .st
            .lock()
            .info
            .parent_id
            .clone()
            .ok_or("The helper's parent is missing.")?;
        let parent = self.rt(&parent).map_err(|e| e.message)?;
        if self.helper_path(&parent.st.lock(), path)? {
            return Err(
                "The worktree metadata or destination is read-only; isolation cannot widen access."
                    .into(),
            );
        }
        Ok(())
    }

    /// Keep what an isolated subagent changed as a patch and remove its
    /// checkout; returns the line shown to the parent.
    async fn collect_worktree(&self, rt: &Rt) -> Option<String> {
        let (id, name, mut wt) = {
            let st = rt.st.lock();
            let s = st.internal.subagent.as_ref()?;
            (st.info.id.clone(), s.name.clone(), s.worktree.clone()?)
        };
        if wt.status != WorktreeStatus::Active {
            return None;
        }
        let path = PathBuf::from(&wt.path);
        let sb = self.session_sandbox(&id);
        let sb = sb.as_ref();
        let d = match worktree::diff(&path, &wt.base, sb).await {
            Ok(d) => d,
            Err(e) => {
                return Some(format!(
                    "Could not collect its worktree changes ({e}); they are still in {}.",
                    wt.path
                ));
            }
        };
        let note = if d.files.is_empty() {
            wt.status = WorktreeStatus::Empty;
            wt.patch = None;
            "Worktree: it changed no files.".to_string()
        } else {
            let Ok(art) = self
                .store
                .put_artifact(Some(&id), &format!("patch {name}"), &d.patch)
            else {
                return Some(format!(
                    "Could not save its worktree changes; they are still in {}.",
                    wt.path
                ));
            };
            wt.patch = Some(art);
            wt.status = WorktreeStatus::Pending;
            wt.files = d.files;
            wt.added = d.added;
            wt.removed = d.removed;
            format!(
                "Changes (in its private worktree, not in your files yet): {} — {}. Review them, then `agents` apply or discard with name \"{name}\".",
                wt.stat(),
                wt.file_list(8)
            )
        };
        worktree::remove(Path::new(&wt.repo), &path, sb).await;
        worktree::unregister(&self.store, &wt.path);
        {
            let mut st = rt.st.lock();
            if let Some(s) = st.internal.subagent.as_mut() {
                s.worktree = Some(wt);
            }
        }
        self.save(rt);
        Some(note)
    }

    /// At startup: checkouts left by a crash. Work a subagent had not handed
    /// back yet is kept as a patch; then the checkout goes.
    pub(super) async fn cleanup_worktrees(&self) {
        let sb = self.sandbox_spec();
        let sb = sb.as_ref();
        for r in worktree::registered(&self.store) {
            let path = PathBuf::from(&r.path);
            let id = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if path.is_dir()
                && let Ok(Some((info, mut internal))) = self.store.get_session(&id)
                && let Some(spec) = internal.subagent.as_mut()
                && let Some(wt) = spec.worktree.as_mut()
                && wt.status == WorktreeStatus::Active
            {
                match worktree::diff(&path, &wt.base, sb).await {
                    Ok(d) if d.files.is_empty() => wt.status = WorktreeStatus::Empty,
                    Ok(d) => match self.store.put_artifact(
                        Some(&id),
                        &format!("patch {}", spec.name),
                        &d.patch,
                    ) {
                        Ok(art) => {
                            wt.patch = Some(art);
                            wt.files = d.files;
                            wt.added = d.added;
                            wt.removed = d.removed;
                            wt.status = WorktreeStatus::Pending;
                        }
                        Err(_) => continue,
                    },
                    Err(e) => {
                        tracing::warn!("keeping worktree {}: {e}", r.path);
                        continue;
                    }
                }
                let _ = self.store.put_session(&info, &internal);
            }
            worktree::remove(Path::new(&r.repo), &path, sb).await;
            worktree::unregister(&self.store, &r.path);
        }
    }

    // ---- running and reporting ---------------------------------------------------

    pub(super) async fn subagents(
        self: Arc<Self>,
        parent: String,
        call_id: String,
        tasks: Vec<SubTask>,
        cancel: CancellationToken,
    ) -> Vec<SubResult> {
        let n = tasks.len();
        let fail = |e: &str| {
            (0..n)
                .map(|_| SubResult {
                    session_id: None,
                    result: Err(e.to_string()),
                    note: None,
                })
                .collect()
        };
        let Ok(parent_rt) = self.rt(&parent) else {
            return fail("parent session not found");
        };
        let plans = match self.plan_tasks(&parent_rt, tasks) {
            Ok(p) => p,
            Err(e) => return fail(&e),
        };
        let futs = plans.into_iter().map(|plan| {
            let me = self.clone();
            let parent_rt = parent_rt.clone();
            let call_id = call_id.clone();
            let cancel = cancel.child_token();
            async move {
                match me.start_child(&parent_rt, &call_id, &plan) {
                    Ok(rt) => {
                        let id = rt.st.lock().info.id.clone();
                        let out = me.clone().finish_child(rt.clone(), cancel).await;
                        SubResult {
                            session_id: Some(id),
                            result: out.result,
                            note: join_notes(
                                join_notes(
                                    plan.note,
                                    Some(me.child_capabilities(&rt, Some(&plan.task.prompt))),
                                ),
                                out.note,
                            ),
                        }
                    }
                    Err(e) => SubResult {
                        session_id: None,
                        result: Err(e),
                        note: plan.note,
                    },
                }
            }
        });
        futures::future::join_all(futs).await
    }

    fn reserve_capacity(
        self: &Arc<Self>,
        parent: &str,
        n: usize,
    ) -> Result<RosterReservation, String> {
        let mut bg = self.background.lock();
        let live = |l: &Vec<BgAgent>| l.iter().filter(|a| !a.done).count();
        let mine = bg.get(parent).map(live).unwrap_or(0);
        let all: usize = bg.values().map(live).sum();
        if mine + n > MAX_BACKGROUND_PER_SESSION {
            return Err(format!(
                "{mine} subagents are still running for this conversation (limit {MAX_BACKGROUND_PER_SESSION}); wait for their reports or stop some with `agents`"
            ));
        }
        if all + n > MAX_BACKGROUND_TOTAL {
            return Err(format!(
                "the server is already running {all} background subagents (limit {MAX_BACKGROUND_TOTAL}); try again once some finish"
            ));
        }
        let ids: Vec<_> = (0..n).map(|_| format!("pending:{}", new_id())).collect();
        let list = bg.entry(parent.to_string()).or_default();
        for id in &ids {
            list.push(BgAgent {
                name: "Pending".into(),
                session_id: id.clone(),
                started: Instant::now(),
                cancel: CancellationToken::new(),
                batch: u64::MAX,
                done: false,
                stopped: false,
            });
        }
        Ok(RosterReservation {
            inner: self.clone(),
            parent: parent.into(),
            ids,
        })
    }

    /// Start subagents in the background: the parent keeps going (and stays
    /// free to answer the user); each report is delivered into the parent's
    /// conversation as it lands, and the parent resumes once the batch is done.
    pub(super) fn spawn_background(
        self: &Arc<Self>,
        parent: &str,
        call_id: &str,
        tasks: Vec<SubTask>,
    ) -> Result<Vec<Started>, String> {
        let parent_rt = self.rt(parent).map_err(|e| e.message)?;
        let mut reservation = self.reserve_capacity(parent, tasks.len())?;
        let plans = self.plan_tasks(&parent_rt, tasks)?;
        let batch = self
            .background_batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut started = Vec::new();
        for plan in plans {
            let rt = self.start_child(&parent_rt, call_id, &plan)?;
            let child = rt.st.lock().info.id.clone();
            let capabilities = self.child_capabilities(&rt, Some(&plan.task.prompt));
            self.run_background(parent, rt, &plan.task.name, batch, &mut reservation);
            let note = match (plan.spec.worktree.is_some(), plan.note) {
                (true, Some(n)) => Some(format!("in a private worktree; {n}")),
                (true, None) => Some("in a private worktree".to_string()),
                (false, n) => n,
            };
            started.push(Started {
                name: plan.task.name,
                session_id: child,
                note: join_notes(
                    Some(format!(
                        "queued — waiting for a provider lane or helper slot; {capabilities}"
                    )),
                    note,
                ),
            });
        }
        Ok(started)
    }

    /// Run a started subagent in the background and deliver its report.
    fn run_background(
        self: &Arc<Self>,
        parent: &str,
        rt: Arc<Rt>,
        name: &str,
        batch: u64,
        reservation: &mut RosterReservation,
    ) {
        let child = rt.st.lock().info.id.clone();
        let cancel = CancellationToken::new();
        let live = self.stops_here();
        {
            let mut st = rt.st.lock();
            st.cancel = Some(cancel.clone());
            st.turn_stops = st.next_turn_stops.clone().unwrap_or(live);
        }
        {
            let claim = reservation.ids.pop().expect("reserved roster slot");
            let mut bg = self.background.lock();
            let list = bg.entry(parent.to_string()).or_default();
            list.retain(|a| a.session_id != child && a.session_id != claim);
            list.push(BgAgent {
                name: name.to_string(),
                session_id: child.clone(),
                started: Instant::now(),
                cancel: cancel.clone(),
                batch,
                done: false,
                stopped: false,
            });
        }
        let me = self.clone();
        let parent = parent.to_string();
        let name = name.to_string();
        tokio::spawn(async move {
            let out = me.clone().finish_child(rt, cancel).await;
            me.deliver_report(&parent, &child, &name, out);
        });
    }

    /// A subagent an update cut off (see `mark_interrupted`): it goes on in
    /// the background and reports to its chat when done, as it would have.
    pub(super) fn continue_background(self: &Arc<Self>, rt: Arc<Rt>) {
        let (parent, name) = {
            let st = rt.st.lock();
            let Some(parent) = st.info.parent_id.clone() else {
                return;
            };
            let name = st
                .internal
                .subagent
                .as_ref()
                .map(|s| s.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| st.info.title.clone());
            (parent, name)
        };
        {
            // It goes on, maybe after waiting its turn: busy, not interrupted.
            let mut st = rt.st.lock();
            st.running = false;
            st.info.status = SessionStatus::Running;
        }
        self.save(&rt);
        let batch = self
            .background_batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut reservation = match self.reserve_capacity(&parent, 1) {
            Ok(r) => r,
            Err(e) => {
                self.notice(&rt, NoticeLevel::Error, e);
                self.set_status(&rt, SessionStatus::Interrupted);
                return;
            }
        };
        self.run_background(&parent, rt, &name, batch, &mut reservation);
    }

    fn deliver_report(self: &Arc<Self>, parent: &str, child: &str, name: &str, out: ChildOutcome) {
        // Pin the report's destination before retiring its live-helper
        // dependency. Eviction must not run between those two boundaries.
        let parent_rt = self.rt(parent).ok();
        let (elapsed, stopped, batch_done, batch_size) = {
            let mut bg = self.background.lock();
            let Some(list) = bg.get_mut(parent) else {
                return;
            };
            let Some(me) = list.iter_mut().find(|a| a.session_id == child) else {
                return;
            };
            if me.done {
                return; // The same result cannot change dependencies twice.
            }
            me.done = true;
            let (elapsed, stopped, batch) = (me.started.elapsed(), me.stopped, me.batch);
            let batch_done = list.iter().filter(|a| a.batch == batch).all(|a| a.done);
            let batch_size = list.iter().filter(|a| a.batch == batch).count();
            if list.iter().all(|a| a.done) {
                bg.remove(parent);
            }
            (elapsed, stopped, batch_done, batch_size)
        };
        let Some(parent_rt) = parent_rt else {
            return;
        };
        // Invalidate before admission even when the report will be queued.
        self.invalidate_completion(&parent_rt);
        let steps = self
            .rt(child)
            .map(|rt| rt.st.lock().info.steps)
            .unwrap_or(0);
        let mins = elapsed.as_secs() / 60;
        let took = if mins > 0 {
            format!("{mins} min")
        } else {
            format!("{} s", elapsed.as_secs())
        };
        let finished = out.result.is_ok();
        let body = match out.result {
            Ok(text) => report_excerpt(text.trim(), batch_size, name, child),
            Err(e) => format!("The subagent did not finish: {e}"),
        };
        // The user's notification center hears of it (its own chat only).
        if !stopped {
            self.helper_reported(parent, name, finished);
        }
        let mut text =
            format!("[Report from subagent \"{name}\" · {steps} steps · {took} · session {child}]");
        if let Some(note) = &out.note {
            text.push('\n');
            text.push_str(note);
        }
        text.push_str("\n\n");
        text.push_str(&body);
        // Reports belong to their helper run, never a newer parent turn; a
        // turn starting meanwhile either reads it or takes it as steering.
        let report = Waiting {
            id: new_id(),
            content: vec![ContentPart::text(text)],
            author: Some(format!("subagent:{child}")),
            stops: out.stops.clone(),
        };
        if !matches!(self.append_or_steer(&parent_rt, report), Ok(Some(_))) {
            return;
        }
        if batch_done && !stopped {
            // A delayed report retains its originating authority even if
            // the child runtime was continued or removed in the meantime.
            if self.running_here_since(&out.stops) {
                self.clone().resume_at(parent_rt, Some(out.stops));
            }
        }
    }

    /// Start a turn on what is already in the history (e.g. arrived
    /// reports), belonging to the Stops of `stops` (else the current).
    pub(super) fn resume_at(self: Arc<Self>, rt: Arc<Rt>, stops: Option<StopMarks>) {
        {
            let mut st = rt.st.lock();
            if st.running {
                return;
            }
            st.running = true;
            st.next_turn_stops = stops;
        }
        tokio::spawn(async move {
            let cancel = CancellationToken::new();
            self.run(rt, cancel).await;
        });
    }

    pub(super) fn agents_status(&self, parent: &str) -> String {
        // Snapshot the live roster before reading sessions: do not hold the
        // background lock across session-map/state locks.
        let live: HashMap<String, (bool, bool, u64)> = self
            .background
            .lock()
            .get(parent)
            .map(|list| {
                list.iter()
                    .map(|a| {
                        (
                            a.session_id.clone(),
                            (a.done, a.stopped, a.started.elapsed().as_secs()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let children: Vec<_> = self
            .store
            .list_sessions(false)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                s.kind == SessionKind::Subagent
                    && !s.internal
                    && s.parent_id.as_deref() == Some(parent)
            })
            .collect();
        if children.is_empty() {
            return "No subagents for this session.".into();
        }
        let waits = self.helper_waits(parent);
        let mut out = format!("{} subagent(s):\n", children.len());
        // Read before any helper's lock: it looks at this chat's own limits.
        let (default_steps, default_minutes) = self.helper_budget(parent);
        for info in children {
            let Ok(rt) = self.rt(&info.id) else { continue };
            let (name, state, steps, changes, budget) = {
                let st = rt.st.lock();
                let spec = st.internal.subagent.as_ref();
                let name = spec
                    .map(|s| s.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| st.info.title.clone());
                let state = if let Some(outcome) =
                    st.internal.attempts.last().and_then(|a| a.outcome)
                {
                    super::attempts::status(outcome).to_string()
                } else {
                    match live.get(&info.id) {
                    Some((_, true, _)) => "stopping/stopped".to_string(),
                    Some((true, _, _)) => "finished (report delivered)".to_string(),
                    Some((false, _, _)) if spec.is_some_and(|s| s.queued) => "queued — waiting for a provider lane or helper slot".to_string(),
                    Some((false, _, _)) => "running".to_string(),
                    None if spec.is_some_and(|s| s.interrupted_by_restart) => "interrupted by restart — inspect its report/files, then use agents message with this session id to continue; do not blindly repeat side effects".to_string(),
                    None => format!("{:?}", st.info.status).to_lowercase(),
                }
                };
                let changes = spec
                    .and_then(|s| s.worktree.as_ref())
                    .filter(|w| w.status == WorktreeStatus::Pending)
                    .map(|w| format!("; worktree changes waiting for apply ({})", w.stat()))
                    .unwrap_or_default();
                let max = spec
                    .and_then(|s| s.max_steps)
                    .unwrap_or(default_steps)
                    .min(default_steps)
                    .max(1);
                let remaining = st.budget.as_ref().map(|b| {
                    b.max_steps
                        .saturating_sub(b.used.max(st.info.steps.saturating_sub(b.base)))
                });
                let unlimited = super::helper_limits::UNLIMITED;
                let budget = match remaining {
                    Some(_) if max == unlimited => "no step limit in this run".to_string(),
                    Some(left) => format!("{left}/{max} tool calls remaining in this run"),
                    None => format!(
                        "run budget: {}, {} (starts after a slot)",
                        super::helper_limits::budget_text(max, "tool calls", "no step limit"),
                        super::helper_limits::budget_text(
                            spec.and_then(|s| s.max_minutes).unwrap_or(default_minutes),
                            "min",
                            "no time limit"
                        )
                    ),
                };
                (name, state, st.info.steps, changes, budget)
            };
            out.push_str(&format!(
                "- {name} ({}) — {state}; {steps} tool calls total; {budget}{changes}\n  {}\n",
                info.id,
                self.child_capabilities(&rt, None)
            ));
            if let Some(wait) = waits.get(&info.id) {
                out.push_str(&format!("  {wait}\n"));
            }
        }
        out
    }

    /// The session id of one of `parent`'s own subagents, by name or id.
    pub(super) fn find_child(&self, parent: &str, which: &str) -> Result<String, String> {
        let which = which.trim();
        let missing = || format!("no subagent `{which}` in this conversation");
        let from_list = self.background.lock().get(parent).and_then(|l| {
            l.iter()
                .rev()
                .find(|a| a.session_id == which || a.name.eq_ignore_ascii_case(which))
                .map(|a| a.session_id.clone())
        });
        let child = match from_list {
            Some(id) => id,
            None => {
                let title = format!("↳ {which}");
                self.store
                    .list_sessions(false)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|s| {
                        s.kind == SessionKind::Subagent
                            && !s.internal
                            && s.parent_id.as_deref() == Some(parent)
                    })
                    .filter(|s| s.id == which || s.title.eq_ignore_ascii_case(&title))
                    .max_by_key(|s| s.created_at)
                    .map(|s| s.id)
                    .ok_or_else(missing)?
            }
        };
        let (info, _) = self
            .store
            .get_session(&child)
            .map_err(|e| e.to_string())?
            .ok_or_else(missing)?;
        if info.kind != SessionKind::Subagent
            || info.internal
            || info.parent_id.as_deref() != Some(parent)
        {
            return Err(missing());
        }
        Ok(child)
    }

    /// The full report of one of `parent`'s subagents, by name or session id.
    /// Only that conversation's own subagents can be read this way.
    pub(super) fn subagent_report(&self, parent: &str, which: &str) -> Result<String, String> {
        let child = self.find_child(parent, which)?;
        let attempt = self
            .rt(&child)
            .map_err(|e| e.message)?
            .st
            .lock()
            .internal
            .attempts
            .last()
            .cloned();
        let entries = self.store.entries(&child, 0).map_err(|e| e.to_string())?;
        let text = super::attempts::history(&entries, attempt.as_ref());
        if attempt.and_then(|a| a.outcome) == Some(crate::store::AttemptOutcome::Completed) {
            Ok(text)
        } else {
            Err(text)
        }
    }

    /// `parent`'s own helpers still at work in the background (queued ones
    /// too): their reports start its next turn.
    pub(super) fn helpers_at_work(&self, parent: &str) -> bool {
        self.background
            .lock()
            .get(parent)
            .is_some_and(|list| list.iter().any(|a| !a.done && !a.stopped))
    }

    /// Stop background subagents of `parent` (all, or those named `name`).
    pub(super) fn stop_background(&self, parent: &str, name: Option<&str>) -> usize {
        let mut n = 0;
        if let Some(list) = self.background.lock().get_mut(parent) {
            for a in list.iter_mut().filter(|a| {
                !a.done && name.is_none_or(|w| a.name.eq_ignore_ascii_case(w) || a.session_id == w)
            }) {
                a.stopped = true;
                a.cancel.cancel();
                n += 1;
            }
        }
        n
    }

    // ---- continue, apply, discard ------------------------------------------------

    pub(super) async fn subagent_control(
        self: Arc<Self>,
        parent: String,
        action: SubAction,
    ) -> Result<String, String> {
        match action {
            SubAction::Message { which, text } => self.message_child(&parent, &which, &text),
            SubAction::Apply { which, seq } => self.apply_child(&parent, &which, seq).await,
            SubAction::Discard { which } => self.discard_child(&parent, &which).await,
            SubAction::Approve { which, pick } => {
                self.answer_helper_approval(&parent, &which, true, pick.as_deref())
            }
            SubAction::Decline { which, pick } => {
                self.answer_helper_approval(&parent, &which, false, pick.as_deref())
            }
        }
    }

    /// A finished (not running) subagent of `parent`, with its label.
    fn idle_child(
        &self,
        parent: &str,
        which: &str,
    ) -> Result<(Arc<Rt>, String, SubagentSpec), String> {
        let child = self.find_child(parent, which)?;
        let rt = self.rt(&child).map_err(|e| e.message)?;
        let busy = self
            .background
            .lock()
            .get(parent)
            .is_some_and(|l| l.iter().any(|a| a.session_id == child && !a.done));
        let (running, title, spec) = {
            let st = rt.st.lock();
            (
                st.running,
                st.info.title.clone(),
                st.internal.subagent.clone().unwrap_or_default(),
            )
        };
        if busy || running {
            return Err(format!(
                "\"{which}\" is still working; its report will arrive on its own"
            ));
        }
        let name = if spec.name.is_empty() {
            title.trim_start_matches("↳ ").to_string()
        } else {
            spec.name.clone()
        };
        Ok((rt, name, spec))
    }

    /// Continue a finished subagent with a follow-up: it keeps its context
    /// (and worktree), gets a fresh budget, and reports again.
    fn message_child(
        self: &Arc<Self>,
        parent: &str,
        which: &str,
        text: &str,
    ) -> Result<String, String> {
        // From the agent that started it (its bot, or the chat), not the user.
        let author = self
            .rt(parent)
            .map(|rt| super::agent_author(&rt.st.lock()))
            .map_err(|e| e.message)?;
        // Still working: the message reaches it at its next step (fresh
        // facts for a running subagent shouldn't wait for its report).
        if let Ok(child) = self.find_child(parent, which)
            && let Ok(rt) = self.rt(&child)
        {
            let mut st = rt.st.lock();
            if st.running {
                self.update_queue(&mut st, |st| {
                    let stops = st.turn_stops.clone();
                    st.steer.push(Waiting {
                        id: new_id(),
                        content: vec![ContentPart::text(format!(
                            "[Message from the agent that started you] {}",
                            text.trim()
                        ))],
                        author: Some(author.clone()),
                        stops,
                    });
                    Ok(())
                })
                .map_err(|e| e.message)?;
                self.queue_changed(&st);
                return Ok(format!(
                    "Delivered to \"{which}\" while it works: it reads this at its next step. Its report will arrive here as usual; don't wait or poll."
                ));
            }
        }
        let (rt, name, spec) = self.idle_child(parent, which)?;
        if spec
            .worktree
            .as_ref()
            .is_some_and(|w| w.status == WorktreeStatus::Discarded)
        {
            return Err(format!(
                "\"{name}\"'s worktree changes were discarded, so it can't pick up where it left off; start a new subagent instead"
            ));
        }
        self.verify_source_scope(&spec)?;
        if spec.worktree.is_none() {
            self.helper_authorized_cwd(&rt)?;
        }
        let mut reservation = self.reserve_capacity(parent, 1)?;
        let parent_rt = self.rt(parent).map_err(|e| e.message)?;
        let (ceiling, parent_stops) = {
            let pst = parent_rt.st.lock();
            (self.effective_permission(&pst), pst.turn_stops.clone())
        };
        let child = {
            let mut st = rt.st.lock();
            // Never more than the parent may do now.
            st.info.permission = crate::tools::cap_permission(st.info.permission, ceiling);
            self.publish_permission(&st);
            st.running = true;
            // Its new turn belongs to the parent's current one.
            st.next_turn_stops = Some(parent_stops);
            st.budget = None;
            st.internal.helper_budget = None;
            st.info.status = SessionStatus::Running;
            if let Some(spec) = st.internal.subagent.as_mut() {
                spec.queued = true;
                spec.interrupted_by_restart = false;
            }
            st.info.id.clone()
        };
        self.recover_interrupted(&rt);
        let mut msg = text.trim().to_string();
        if spec.output.is_some() {
            msg.push_str("\n\n(Reply with only the JSON value, in the format asked for before.)");
        }
        if let Err(e) = self.append(
            &rt,
            EntryBody::User {
                content: vec![ContentPart::text(msg)],
                author: Some(author),
            },
        ) {
            rt.st.lock().running = false;
            return Err(e.to_string());
        }
        let batch = self
            .background_batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let capabilities = self.child_capabilities(&rt, Some(text));
        self.run_background(parent, rt, &name, batch, &mut reservation);
        Ok(format!(
            "Sent to \"{name}\" (session {child}), queued for a provider lane or helper slot. {capabilities}\nIt continues with its earlier context; its new report will arrive in this conversation. Don't wait or poll."
        ))
    }

    /// An isolated subagent's finished work, if it is waiting to be applied.
    fn pending_work(name: &str, spec: &SubagentSpec) -> Result<WorktreeState, String> {
        let wt = spec.worktree.clone().ok_or_else(|| {
            format!("\"{name}\" didn't work in a worktree; its changes are already in your files")
        })?;
        match wt.status {
            WorktreeStatus::Pending => Ok(wt),
            WorktreeStatus::Empty => Err(format!(
                "\"{name}\" changed no files; there is nothing to apply"
            )),
            WorktreeStatus::Applied => Err(format!("\"{name}\"'s changes were already applied")),
            WorktreeStatus::Discarded => Err(format!("\"{name}\"'s changes were discarded")),
            WorktreeStatus::New | WorktreeStatus::Active => {
                Err(format!("\"{name}\" hasn't finished yet"))
            }
        }
    }

    fn set_worktree(&self, rt: &Rt, wt: WorktreeState) {
        {
            let mut st = rt.st.lock();
            if let Some(s) = st.internal.subagent.as_mut() {
                s.worktree = Some(wt);
            }
        }
        self.save(rt);
    }

    /// Bring an isolated subagent's changes into the parent's files, all or
    /// nothing: on any conflict nothing changes.
    async fn apply_child(
        self: &Arc<Self>,
        parent: &str,
        which: &str,
        seq: u64,
    ) -> Result<String, String> {
        let (rt, name, spec) = self.idle_child(parent, which)?;
        let mut wt = Self::pending_work(&name, &spec)?;
        let parent_rt = self.rt(parent).map_err(|e| e.message)?;
        let admission = {
            let st = parent_rt.st.lock();
            self.action_admission(&st)
        };
        if self.effective_permission(&parent_rt.st.lock()) == PermissionMode::ReadOnly {
            return Err("This session is read-only; the changes were not applied.".into());
        }
        let art = wt.patch.clone().unwrap_or_default();
        let patch = self
            .store
            .artifact(&art)
            .ok()
            .flatten()
            .map(|(_, c)| c)
            .ok_or_else(|| format!("\"{name}\"'s patch is missing"))?;
        let top = PathBuf::from(&wt.repo);
        let sb = self.session_sandbox(parent);
        let sb = sb.as_ref();
        if let Err(e) = worktree::check(&top, &patch, sb).await {
            return Err(format!(
                "Not applied: \"{name}\"'s changes conflict with the current files ({e}). Nothing was changed. \
                 The patch is artifact `{art}` (read it with `recall`): apply the parts you want by hand, or discard it."
            ));
        }
        // Rewinding this conversation can then undo the apply.
        let ctx = ToolCtx {
            session_id: parent.into(),
            call_id: String::new(),
            admission: admission.clone(),
            seq,
            cwd: top.clone(),
            cancel: CancellationToken::new(),
            host: Arc::new(Host(self.clone(), false)),
            progress: Arc::new(|_| {}),
            depth: 0,
            cleared: None,
        };
        for f in &wt.files {
            let p = top.join(f);
            super::claude_code::snapshot_before_edit(&ctx, &p)?;
        }
        worktree::admitted(admission, worktree::apply(&top, &patch, sb))
            .await
            .map_err(|e| format!("Not applied: {e}"))?;
        wt.status = WorktreeStatus::Applied;
        let (stat, files) = (wt.stat(), wt.file_list(12));
        self.set_worktree(&rt, wt);
        Ok(format!(
            "Applied \"{name}\"'s changes to {}: {stat} — {files}. They are unstaged changes in the working tree.",
            top.display()
        ))
    }

    async fn discard_child(&self, parent: &str, which: &str) -> Result<String, String> {
        let (rt, name, spec) = self.idle_child(parent, which)?;
        let mut wt = match Self::pending_work(&name, &spec) {
            Ok(wt) => wt,
            Err(_)
                if spec
                    .worktree
                    .as_ref()
                    .is_some_and(|w| w.status == WorktreeStatus::Empty) =>
            {
                spec.worktree.clone().unwrap_or_default()
            }
            Err(e) => return Err(e),
        };
        let path = PathBuf::from(&wt.path);
        if path.exists() {
            let sb = self.session_sandbox(parent);
            worktree::remove(Path::new(&wt.repo), &path, sb.as_ref()).await;
            worktree::unregister(&self.store, &wt.path);
        }
        wt.status = WorktreeStatus::Discarded;
        let stat = wt.stat();
        self.set_worktree(&rt, wt);
        Ok(format!(
            "Discarded \"{name}\"'s changes ({stat}). Your files were not touched."
        ))
    }
}

/// Best-effort hints for common assignment wording, never authorization.
fn capability_warnings(prompt: &str, tools: &[String], readonly: bool) -> Vec<&'static str> {
    let p = prompt.to_ascii_lowercase();
    let has = |name: &str| tools.iter().any(|t| t == name);
    let mentions = |words: &[&str]| words.iter().any(|w| p.contains(w));
    let remote = mentions(&["machine", "remote", "machines."]);
    let mut out = Vec::new();
    if mentions(&["write", "save", "create", "edit", "persist"])
        && mentions(&["file", "artifact", ".md", "report", "notes"])
        && !(has("write") || has("edit") || (remote && has("machines") && !readonly))
    {
        out.push("this task appears to request file writes, but this helper has no file-write tools. Return the evidence to the parent to save, or select a write-capable role.");
    }
    if remote && !has("machines") {
        out.push("this task mentions a remote machine, but this helper has no machines tool. Supply a local evidence bundle or choose an authorized machine-capable role.");
    }
    if mentions(&[
        "run ",
        "execute",
        "shell",
        "bash",
        "sha256",
        "cargo ",
        "compute hash",
    ]) && if remote {
        !has("machines") || readonly
    } else {
        !has("bash") && !has(crate::tools::bash::READ_ONLY_BASH)
    } {
        out.push("this task appears to need command execution, but this helper has no command runner (its type's tool list leaves none).");
    }
    out
}

fn join_digest(why: String, digest: Option<String>) -> String {
    match digest {
        Some(d) => format!("{why}.\n\n{d}"),
        None => why,
    }
}

/// What a subagent did, from its history (for when it wrote no report):
/// files it changed, its last commands and how they ended.
fn activity_digest(entries: &[Entry]) -> Option<String> {
    let mut calls = 0usize;
    let mut changed: Vec<String> = Vec::new();
    // (call id, command, how it ended)
    let mut commands: Vec<(String, String, &str)> = Vec::new();
    for e in entries {
        match &e.body {
            EntryBody::Assistant { content, .. } => {
                for p in content {
                    let AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                    } = p
                    else {
                        continue;
                    };
                    calls += 1;
                    let arg = |k: &str| arguments.get(k).and_then(Value::as_str);
                    match name.as_str() {
                        "edit" | "write" => {
                            if let Some(path) = arg("path")
                                && !changed.iter().any(|c| c == path)
                            {
                                changed.push(path.to_string());
                            }
                        }
                        "bash" => {
                            if let Some(c) = arg("command") {
                                commands.push((id.clone(), one_line(c, 100), "no result"));
                            }
                        }
                        _ => {}
                    }
                }
            }
            EntryBody::ToolResult {
                call_id,
                is_error,
                details,
                ..
            } => {
                if let Some(c) = commands.iter_mut().find(|(id, _, _)| id == call_id) {
                    let code = details
                        .as_ref()
                        .and_then(|d| d.get("exit_code"))
                        .and_then(Value::as_i64);
                    c.2 = match (code, is_error) {
                        (Some(0), _) | (None, false) => "ok",
                        _ => "failed",
                    };
                }
            }
            _ => {}
        }
    }
    if calls == 0 {
        return None;
    }
    let mut out = format!("What its history shows ({calls} tool calls):");
    if !changed.is_empty() {
        let more = changed.len().saturating_sub(12);
        changed.truncate(12);
        out.push_str(&format!("\n- Changed: {}", changed.join(", ")));
        if more > 0 {
            out.push_str(&format!(" (+{more} more)"));
        }
    }
    let last: Vec<String> = commands
        .iter()
        .rev()
        .take(4)
        .rev()
        .map(|(_, c, how)| format!("`{c}` ({how})"))
        .collect();
    if !last.is_empty() {
        out.push_str(&format!("\n- Last commands: {}", last.join("; ")));
    }
    out.push_str("\nCheck the actual state before relying on or redoing any of it.");
    Some(out)
}

/// The newest assistant text in a session: a subagent's report.
fn last_answer(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| !crate::tools::meta::is_protocol_entry(e))
        .find_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => {
                let t: String = content
                    .iter()
                    .filter_map(|p| {
                        if let AssistantPart::Text { text } = p {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                (!t.trim().is_empty() && !crate::tools::meta::is_check_ack(&t)).then_some(t)
            }
            _ => None,
        })
}

/// A swarm's reports must not flood the parent's context: each gets a share
/// of a fixed budget, and the rest stays readable with `agents report`.
/// A JSON report (structured output) is never cut: half a JSON value is
/// useless.
fn report_excerpt(text: &str, batch_size: usize, name: &str, child: &str) -> String {
    let cap = (64_000 / batch_size.max(1)).clamp(4_000, 24_000);
    if text.len() <= cap
        || (text.len() <= 64_000
            && text.starts_with(['{', '['])
            && serde_json::from_str::<Value>(text).is_ok())
    {
        return text.to_string();
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    // Prefer ending at a paragraph or line break.
    let head = &text[..cut];
    let head = head
        .rfind("\n\n")
        .or_else(|| head.rfind('\n'))
        .filter(|&i| i > cut / 2)
        .map_or(head, |i| &head[..i]);
    format!(
        "{}\n\n[… {} more characters. Read the full report with `agents` action `report`, name \"{name}\" (session {child}).]",
        head.trim_end(),
        text.len() - head.len()
    )
}

/// A helper's folder identities ([`Inner::helper_identities`]): the folder it
/// was started for, and the one it works in (its worktree, or the same).
pub(super) struct HelperFolder {
    source: DirectoryScope,
    active: DirectoryScope,
}

impl HelperFolder {
    /// Both still the checked folders at their places.
    fn verified(&self) -> Result<&DirectoryScope, String> {
        self.source.verify()?;
        if self.active != self.source {
            self.active.verify()?;
        }
        Ok(&self.active)
    }
}

#[cfg(test)]
impl Inner {
    pub(super) fn start_lock_order_test_child(&self, parent: &Rt) -> Arc<Rt> {
        let task = SubTask {
            name: "lock-order fixture".into(),
            prompt: "Synthetic task; creation only, never run a model.".into(),
            model: Some(parent.st.lock().info.model.to_string()),
            ..Default::default()
        };
        let plans = self.plan_tasks(parent, vec![task]).unwrap();
        let child = self.start_child(parent, "fixture-call", &plans[0]).unwrap();
        super::test_support::admit_helper(&child);
        child
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_batch_reservations_cannot_cross_unfinished_count_ceiling() {
        let (engine, _dir, _rt) = super::super::test_support::engine().await;
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let inner = engine.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    inner.reserve_capacity("parent", 32)
                })
            })
            .collect();
        let held: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter_map(Result::ok)
            .collect();
        assert_eq!(held.len(), 2);
        assert_eq!(engine.0.background.lock()["parent"].len(), 64);
        drop(held);
        assert!(engine.0.background.lock().get("parent").is_none());
        assert!(engine.0.reserve_capacity("parent", 64).is_ok());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn helper_host_retains_the_expected_launch_object_through_a_cwd_swap() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = project.path().join("selected");
        std::fs::create_dir_all(path.join("sub")).unwrap();
        std::fs::write(path.join("marker"), "checked").unwrap();
        std::fs::write(outside.path().join("marker"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), path.join("away")).unwrap();
        parent.st.lock().info.cwd = path.display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let host = Host(engine.0.clone(), false);
        let sid = child.st.lock().info.id.clone();
        // A command folder below the helper's is reached from it; a link
        // out of it is not.
        let sub = host.process_directory(&sid, &path.join("sub")).unwrap();
        assert_eq!(sub.path(), path.join("sub"));
        assert!(host.process_directory(&sid, &path.join("away")).is_err());
        assert!(host.process_directory(&sid, outside.path()).is_err());
        let directory = host.process_directory(&sid, &path).unwrap();
        std::fs::rename(&path, project.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();
        assert!(
            host.process_directory(&sid, &path).is_err(),
            "a later call cannot recapture the replacement"
        );
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "cat marker"]);
        let _held = directory.apply(&mut cmd, None);
        let out = cmd.output().await.unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"checked");
    }

    #[tokio::test]
    async fn legacy_helper_without_bound_cwd_identity_is_not_reauthorized_on_reload() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let id = {
            let mut st = child.st.lock();
            st.running = false;
            st.internal.subagent.as_mut().unwrap().cwd_scope = None;
            st.info.id.clone()
        };
        engine.0.save(&child);
        engine.0.sessions.lock().remove(&id);
        let loaded = engine.0.rt(&id).unwrap();
        let err = engine.0.helper_authorized_cwd(&loaded).unwrap_err();
        assert!(err.contains("no bound working-folder identity"), "{err}");
        let pid = parent.st.lock().info.id.clone();
        assert!(engine.0.message_child(&pid, &id, "continue").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn active_worktree_cwd_replacement_never_grants_the_new_target_write_scope() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git_fixture(project.path(), &["init", "-q"]);
        git_fixture(project.path(), &["commit", "--allow-empty", "-qm", "base"]);
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = isolated_child(&engine, &parent);
        ensure_test_worktree(&engine, &child).await.unwrap();
        let cwd = PathBuf::from(&child.st.lock().info.cwd);
        let saved = cwd.with_extension("saved");
        std::fs::write(outside.path().join("sentinel.txt"), "unchanged").unwrap();
        std::fs::rename(&cwd, &saved).unwrap();
        std::os::unix::fs::symlink(outside.path(), &cwd).unwrap();
        assert!(engine.0.helper_authorized_cwd(&child).is_err());
        let write = engine.0.registry.read().get("write").unwrap();
        let out = engine
            .0
            .execute_tool(
                &child,
                write,
                "replaced",
                json!({"path":"sentinel.txt","content":"bad"}),
                0,
                &CancellationToken::new(),
            )
            .await;
        assert!(out.is_error, "{:?}", out.content);
        assert!(ensure_test_worktree(&engine, &child).await.is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
            "unchanged"
        );
        std::fs::remove_file(&cwd).unwrap();
        std::fs::rename(&saved, &cwd).unwrap();
        assert!(engine.0.helper_authorized_cwd(&child).is_ok());
        engine.0.collect_worktree(&child).await.unwrap();
    }

    fn git_fixture(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    async fn ensure_test_worktree(engine: &Engine, child: &Rt) -> Result<(), String> {
        let admission = engine.0.action_admission(&child.st.lock());
        engine.0.ensure_worktree(child, admission).await
    }
    fn isolated_child(engine: &Engine, parent: &Rt) -> Arc<Rt> {
        let plan = engine
            .0
            .plan_tasks(
                parent,
                vec![SubTask {
                    name: "scope regression".into(),
                    prompt: "inspect files".into(),
                    worktree: true,
                    ..Default::default()
                }],
            )
            .unwrap()
            .remove(0);
        engine.0.start_child(parent, "scope-test", &plan).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_rejects_external_common_directory_from_git_file_or_link() {
        for linked in [false, true] {
            let (engine, _store, parent) = super::super::test_support::engine().await;
            let project = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let cwd = project.path().join("repo");
            std::fs::create_dir(&cwd).unwrap();
            if linked {
                git_fixture(external.path(), &["init", "-q"]);
                git_fixture(external.path(), &["commit", "--allow-empty", "-qm", "base"]);
                git_fixture(
                    external.path(),
                    &["worktree", "add", "--detach", cwd.to_str().unwrap()],
                );
                assert!(cwd.join(".git").is_file());
            } else {
                git_fixture(&cwd, &["init", "-q"]);
                git_fixture(&cwd, &["commit", "--allow-empty", "-qm", "base"]);
                std::fs::rename(cwd.join(".git"), external.path().join("metadata")).unwrap();
                std::os::unix::fs::symlink(external.path().join("metadata"), cwd.join(".git"))
                    .unwrap();
            }
            parent.st.lock().info.cwd = project.path().display().to_string();
            let plan = engine
                .0
                .plan_tasks(
                    &parent,
                    vec![SubTask {
                        name: "escape".into(),
                        prompt: "inspect".into(),
                        cwd: Some("repo".into()),
                        worktree: true,
                        ..Default::default()
                    }],
                )
                .unwrap()
                .remove(0);
            let child = engine.0.start_child(&parent, "escape", &plan).unwrap();
            let before = worktree::repo(&cwd, None).await.unwrap();
            let before_list = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["worktree", "list", "--porcelain"])
                .output()
                .unwrap()
                .stdout;
            let err = ensure_test_worktree(&engine, &child).await.unwrap_err();
            assert!(err.contains("outside this chat's folder scope"), "{err}");
            assert!(worktree::registered(&engine.0.store).is_empty());
            assert!(!before.common.join("tessera").exists());
            assert_eq!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&cwd)
                    .args(["worktree", "list", "--porcelain"])
                    .output()
                    .unwrap()
                    .stdout,
                before_list
            );
        }
    }

    /// With the command sandbox off this is the host's own metadata check;
    /// with it on (where this computer has it) Git runs inside it, which
    /// may refuse first (it can't see Tessera's hidden data): either way
    /// nothing is created.
    #[tokio::test]
    async fn worktree_rejects_protected_and_readonly_metadata_before_creating_anything() {
        // (protected, sandbox)
        let mut cases = vec![(false, false), (true, false)];
        if crate::sandbox::test_mode("worktree metadata check in the sandbox").is_some() {
            cases.extend([(false, true), (true, true)]);
        }
        for (protected, sandbox) in cases {
            let (engine, _store, parent) = super::super::test_support::engine().await;
            engine.0.config.write().security.sandbox = if sandbox {
                SandboxMode::Auto
            } else {
                SandboxMode::Off
            };
            let project = tempfile::tempdir().unwrap();
            let external = tempfile::tempdir().unwrap();
            let metadata = if protected {
                engine.0.paths.data_dir.join("git-metadata")
            } else {
                external.path().join("git-metadata")
            };
            git_fixture(
                project.path(),
                &[
                    "init",
                    "-q",
                    "--separate-git-dir",
                    metadata.to_str().unwrap(),
                ],
            );
            git_fixture(project.path(), &["commit", "--allow-empty", "-qm", "base"]);
            parent.st.lock().info.cwd = project.path().display().to_string();
            engine.0.config.write().security.folders.push(FolderAccess {
                path: metadata.display().to_string(),
                write: false,
            });
            let child = isolated_child(&engine, &parent);
            let err = ensure_test_worktree(&engine, &child).await.unwrap_err();
            if !(sandbox && protected) {
                assert!(
                    err.contains(if protected { "protected" } else { "read-only" }),
                    "sandbox {sandbox}: {err}"
                );
            }
            assert!(!metadata.join("tessera").exists(), "sandbox {sandbox}");
            assert!(worktree::registered(&engine.0.store).is_empty());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_recreation_rechecks_redirected_destination_and_common_identity() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        git_fixture(project.path(), &["init", "-q"]);
        git_fixture(project.path(), &["commit", "--allow-empty", "-qm", "base"]);
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = isolated_child(&engine, &parent);
        ensure_test_worktree(&engine, &child).await.unwrap();
        let old = child
            .st
            .lock()
            .internal
            .subagent
            .as_ref()
            .unwrap()
            .worktree
            .clone()
            .unwrap();
        assert!(engine.0.helper_authorized_cwd(&child).is_ok());
        engine.0.collect_worktree(&child).await.unwrap();
        assert!(!Path::new(&old.path).exists());
        let prefix = project.path().join(".git/tessera");
        std::fs::rename(&prefix, project.path().join(".git/saved-tessera")).unwrap();
        std::os::unix::fs::symlink(external.path(), &prefix).unwrap();
        let err = ensure_test_worktree(&engine, &child).await.unwrap_err();
        assert!(err.contains("outside this chat's folder scope"), "{err}");
        assert!(!external.path().join("worktrees").exists());
        assert!(worktree::registered(&engine.0.store).is_empty());
        std::fs::remove_file(&prefix).unwrap();
        std::fs::rename(project.path().join(".git/saved-tessera"), &prefix).unwrap();
        // Authorized ordinary recreation binds a fresh checkout identity.
        ensure_test_worktree(&engine, &child).await.unwrap();
        assert!(engine.0.helper_authorized_cwd(&child).is_ok());
        engine.0.collect_worktree(&child).await.unwrap();
        std::fs::rename(
            project.path().join(".git"),
            external.path().join("metadata"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            external.path().join("metadata"),
            project.path().join(".git"),
        )
        .unwrap();
        let err = ensure_test_worktree(&engine, &child).await.unwrap_err();
        assert!(
            err.contains("location changed") || err.contains("outside"),
            "{err}"
        );
        assert!(!Path::new(&old.path).exists());
        assert!(worktree::registered(&engine.0.store).is_empty());
    }

    #[tokio::test]
    async fn worktree_allows_explicit_writable_metadata_but_not_a_changed_object() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let metadata = external.path().join("metadata");
        git_fixture(
            project.path(),
            &[
                "init",
                "-q",
                "--separate-git-dir",
                metadata.to_str().unwrap(),
            ],
        );
        git_fixture(project.path(), &["commit", "--allow-empty", "-qm", "base"]);
        parent.st.lock().info.cwd = project.path().display().to_string();
        engine.0.config.write().security.folders.push(FolderAccess {
            path: metadata.display().to_string(),
            write: true,
        });
        let child = isolated_child(&engine, &parent);
        ensure_test_worktree(&engine, &child).await.unwrap();
        assert!(engine.0.helper_authorized_cwd(&child).is_ok());
        engine.0.collect_worktree(&child).await.unwrap();
        // Replacing even an authorized directory by a copy invalidates its
        // saved object identity rather than silently reauthorizing it.
        let saved = external.path().join("saved");
        std::fs::rename(&metadata, &saved).unwrap();
        std::fs::create_dir(&metadata).unwrap();
        fn copy_dir(from: &Path, to: &Path) {
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                let dst = to.join(e.file_name());
                if e.file_type().unwrap().is_dir() {
                    std::fs::create_dir(&dst).unwrap();
                    copy_dir(&e.path(), &dst);
                } else {
                    std::fs::copy(e.path(), dst).unwrap();
                }
            }
        }
        copy_dir(&saved, &metadata);
        let err = ensure_test_worktree(&engine, &child).await.unwrap_err();
        assert!(err.contains("folder changed"), "{err}");
        assert!(worktree::registered(&engine.0.store).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selected_helper_cwd_cannot_be_replaced_between_plan_launch_queue_and_continuation() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = project.path().join("selected");
        std::fs::create_dir(&cwd).unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let plan = engine
            .0
            .plan_tasks(
                &parent,
                vec![SubTask {
                    name: "cwd".into(),
                    prompt: "inspect".into(),
                    cwd: Some("selected".into()),
                    ..Default::default()
                }],
            )
            .unwrap()
            .remove(0);
        std::fs::rename(&cwd, project.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &cwd).unwrap();
        assert!(engine.0.start_child(&parent, "late-start", &plan).is_err());
        std::fs::remove_file(&cwd).unwrap();
        std::fs::rename(project.path().join("saved"), &cwd).unwrap();
        let child = engine.0.start_child(&parent, "queued", &plan).unwrap();
        std::fs::rename(&cwd, project.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &cwd).unwrap();
        let result = engine
            .0
            .clone()
            .finish_child(child.clone(), CancellationToken::new())
            .await;
        let failure = result.result.unwrap_err();
        assert!(failure.contains("folder changed"), "{failure}");
        assert!(
            failure.starts_with(super::super::FAILED_REPLY_NOTICE),
            "{failure}"
        );
        assert!(
            failure.contains("Failed: the working folder changed"),
            "{failure}"
        );
        let a = child.st.lock().internal.attempts.last().cloned().unwrap();
        assert_eq!(a.outcome, Some(crate::store::AttemptOutcome::Failed));
        assert!(a.to_seq.is_some());
        assert_eq!(child.st.lock().info.steps, 0);
        let id = child.st.lock().info.id.clone();
        let pid = parent.st.lock().info.id.clone();
        assert!(engine.0.message_child(&pid, &id, "continue").is_err());
        // Removing/reloading the runtime must retain the original inode.
        engine.0.sessions.lock().remove(&id);
        let loaded = engine.0.rt(&id).unwrap();
        assert!(engine.0.helper_authorized_cwd(&loaded).is_err());
        // Same path, replacement real directory, no symlink: still not the
        // selected object. Restoration of the original object is allowed.
        std::fs::remove_file(&cwd).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        assert!(engine.0.helper_authorized_cwd(&loaded).is_err());
        std::fs::remove_dir(&cwd).unwrap();
        std::fs::rename(project.path().join("saved"), &cwd).unwrap();
        assert!(engine.0.helper_authorized_cwd(&loaded).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn helper_write_scope_uses_bound_directory_not_recanonicalized_or_lexical_cwd() {
        let (engine, _store, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = project.path().join("selected");
        std::fs::create_dir(&cwd).unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let plan = engine
            .0
            .plan_tasks(
                &parent,
                vec![SubTask {
                    name: "write".into(),
                    prompt: "write".into(),
                    cwd: Some("selected".into()),
                    ..Default::default()
                }],
            )
            .unwrap()
            .remove(0);
        let child = engine.0.start_child(&parent, "write", &plan).unwrap();
        super::super::test_support::admit_helper(&child);
        let host: Arc<dyn ToolHost> = Arc::new(Host(engine.0.clone(), false));
        let (session_id, admission) = {
            let st = child.st.lock();
            (st.info.id.clone(), engine.0.action_admission(&st))
        };
        let ctx = ToolCtx {
            session_id,
            admission,
            call_id: "direct".into(),
            seq: 0,
            cwd: cwd.clone(),
            cancel: CancellationToken::new(),
            host,
            progress: Arc::new(|_| {}),
            depth: 1,
            cleared: None,
        };
        let write = engine.0.registry.read().get("write").unwrap();
        let ok = engine
            .0
            .execute_tool(
                &child,
                write.clone(),
                "normal",
                json!({"path":"local.txt","content":"local"}),
                0,
                &CancellationToken::new(),
            )
            .await;
        assert!(!ok.is_error, "{:?}", ok.content);
        std::fs::write(outside.path().join("sentinel.txt"), "do not change").unwrap();
        std::os::unix::fs::symlink(outside.path(), cwd.join("alias")).unwrap();
        let risk = crate::tools::files::path_write_risk(&cwd.join("alias/sentinel.txt"), &ctx);
        assert!(matches!(risk, crate::tools::Risk::Ask { .. }), "{risk:?}");
        std::fs::rename(&cwd, project.path().join("saved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &cwd).unwrap();
        assert!(matches!(
            crate::tools::files::path_write_risk(&outside.path().join("sentinel.txt"), &ctx),
            crate::tools::Risk::Block { .. }
        ));
        for args in [
            json!({"path":"sentinel.txt","content":"bad"}),
            json!({"path":outside.path().join("sentinel.txt"),"content":"bad"}),
        ] {
            let out = engine
                .0
                .execute_tool(
                    &child,
                    write.clone(),
                    "replaced",
                    args,
                    0,
                    &CancellationToken::new(),
                )
                .await;
            assert!(out.is_error, "{:?}", out.content);
        }
        // Bypassing dispatch still reaches the file tool's identity check.
        assert!(
            write
                .run(json!({"path":"sentinel.txt","content":"bad"}), &ctx)
                .await
                .is_error
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
            "do not change"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("saved/local.txt")).unwrap(),
            "local"
        );
    }

    #[tokio::test]
    async fn task_cwd_is_preflighted_with_no_escape_or_batch_side_effects() {
        let (engine, dir, parent) = super::super::test_support::engine().await;
        let work = tempfile::tempdir().unwrap();
        let cwd = crate::platform::canonicalize(work.path()).unwrap();
        parent.st.lock().info.cwd = cwd.display().to_string();
        let repo = cwd.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let task = SubTask {
            name: "repo task".into(),
            prompt: "inspect".into(),
            cwd: Some("repo".into()),
            worktree: true,
            ..Default::default()
        };
        let plan = engine
            .0
            .plan_tasks(&parent, vec![task.clone()])
            .unwrap()
            .remove(0);
        let child = engine.0.start_child(&parent, "cwd-test", &plan).unwrap();
        let note = engine.0.child_capabilities(&child, None);
        assert!(
            note.contains("planned worktree:") && note.contains("created after slot"),
            "{note}"
        );
        assert!(note.contains("repo/.git/tessera/worktrees/"), "{note}");
        assert_eq!(
            Path::new(&child.st.lock().info.cwd),
            crate::platform::canonicalize(&repo).unwrap()
        );
        for raw in [
            dir.path().display().to_string(),
            "missing".into(),
            "..".into(),
        ] {
            let mut invalid = task.clone();
            invalid.cwd = Some(raw);
            assert!(
                engine
                    .0
                    .plan_tasks(&parent, vec![task.clone(), invalid])
                    .is_err()
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), cwd.join("escape")).unwrap();
            let mut invalid = task;
            invalid.cwd = Some("escape".into());
            assert!(engine.0.plan_tasks(&parent, vec![invalid]).is_err());
        }
    }

    #[test]
    fn follow_up_report_keeps_the_full_earlier_report() {
        let entries = [
            said(1, "Full findings: parser.rs:10 and all evidence."),
            said(3, "Short follow-up."),
        ];
        let a = crate::store::Attempt {
            id: "fixture".into(),
            safe_cause: None,
            from_seq: 2,
            to_seq: Some(3),
            outcome: Some(crate::store::AttemptOutcome::Completed),
        };
        let report = super::super::attempts::history(&entries, Some(&a));
        assert!(report.contains("Earlier report (historical)"), "{report}");
        assert!(
            report.contains("Full findings: parser.rs:10 and all evidence."),
            "{report}"
        );
        assert!(report.find("Short follow-up").unwrap() < report.find("Full findings").unwrap());
        assert_eq!(
            super::super::attempts::history(
                &entries[..1],
                Some(&crate::store::Attempt {
                    id: "fixture".into(),
                    safe_cause: None,
                    from_seq: 0,
                    to_seq: Some(1),
                    outcome: Some(crate::store::AttemptOutcome::Completed)
                })
            ),
            "Full findings: parser.rs:10 and all evidence."
        );
        assert!(
            super::super::attempts::history(&[], None).contains("no confirmed terminal result")
        );
    }

    #[test]
    fn failed_readback_never_uses_an_old_success_or_provider_notice_as_current_notes() {
        for author in ["harness", "parent"] {
            for partial in ["", "Latest partial notes."] {
                let mut failed = said(3, partial);
                if let EntryBody::Assistant { stop, error, .. } = &mut failed.body {
                    *stop = StopReason::Error;
                    *error = Some("provider-error-private-sentinel".into());
                }
                let entries = [
                    said(1, "Earlier successful report."),
                    Entry {
                        seq: 2,
                        ts: 0,
                        body: EntryBody::User {
                            content: vec![ContentPart::text("Retry or continuation")],
                            author: Some(author.into()),
                        },
                    },
                    failed,
                    Entry {
                        seq: 4,
                        ts: 0,
                        body: EntryBody::Notice {
                            level: NoticeLevel::Error,
                            text: "provider-error-private-sentinel".into(),
                            learned: vec![],
                        },
                    },
                ];
                let a = crate::store::Attempt {
                    id: "fixture".into(),
                    safe_cause: None,
                    from_seq: 2,
                    to_seq: Some(4),
                    outcome: Some(crate::store::AttemptOutcome::Failed),
                };
                let text = super::super::attempts::history(&entries, Some(&a));
                assert!(
                    text.starts_with(super::super::FAILED_REPLY_NOTICE),
                    "{text}"
                );
                assert!(!text.contains("provider-error-private-sentinel"), "{text}");
                assert!(text.contains("Earlier report (historical)"), "{text}");
                let current = text.split("## Earlier").next().unwrap();
                assert!(!current.contains("Earlier successful report."), "{text}");
                if partial.is_empty() {
                    assert!(!current.contains("Partial notes,"), "{text}");
                } else {
                    assert!(
                        current.contains("Partial notes, not a final reply or report:"),
                        "{text}"
                    );
                    assert!(current.contains(partial), "{text}");
                }
            }
        }
        assert_eq!(
            super::super::attempts::history(
                &[],
                Some(&crate::store::Attempt {
                    id: "fixture".into(),
                    safe_cause: None,
                    from_seq: 0,
                    to_seq: Some(0),
                    outcome: Some(crate::store::AttemptOutcome::Failed)
                })
            ),
            super::super::FAILED_REPLY_NOTICE
        );
    }

    #[tokio::test]
    async fn setup_notices_keep_one_attempt_id_and_terminal_status_ignores_idle() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let id = engine.0.begin_attempt(&child).unwrap();
        engine
            .0
            .notice(&child, NoticeLevel::Info, "Safe setup notice.");
        assert_eq!(engine.0.begin_attempt(&child).as_deref(), Some(id.as_str()));
        assert_eq!(child.st.lock().internal.attempts.len(), 1);
        assert!(engine.0.set_attempt_outcome(
            &child,
            &id,
            crate::store::AttemptOutcome::Exhausted,
            None
        ));
        engine.0.set_status(&child, SessionStatus::Idle);
        let parent_id = parent.st.lock().info.id.clone();
        let status = engine.0.agents_status(&parent_id);
        assert!(status.contains("out of steps"), "{status}");
        assert!(!status.contains("— idle"), "{status}");
        let next = engine.0.begin_attempt(&child).unwrap();
        assert_ne!(next, id);
        assert_eq!(child.st.lock().internal.attempts.len(), 2);
    }

    #[tokio::test]
    async fn restart_closes_pending_attempt_with_boundary_and_shared_readback() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let id = engine.0.begin_attempt(&child).unwrap();
        engine
            .0
            .append(&child, said(0, "Restart partial notes.").body)
            .unwrap();
        engine.0.set_status(&child, SessionStatus::Running);
        let child_id = child.st.lock().info.id.clone();
        let end = child.st.lock().info.last_seq;
        engine.0.mark_interrupted(false).unwrap();
        let (_, persisted) = engine.0.store.get_session(&child_id).unwrap().unwrap();
        let a = persisted.attempts.iter().find(|a| a.id == id).unwrap();
        assert_eq!(a.outcome, Some(crate::store::AttemptOutcome::Interrupted));
        assert_eq!(a.to_seq, Some(end));
        let entries = engine.0.store.entries(&child_id, 0).unwrap();
        let text = super::super::attempts::history(&entries, Some(a));
        assert!(
            text.starts_with("This attempt was interrupted by a server restart"),
            "{text}"
        );
        assert!(text.contains("Restart partial notes."), "{text}");
        engine.0.mark_interrupted(false).unwrap();
        let (_, again) = engine.0.store.get_session(&child_id).unwrap().unwrap();
        assert_eq!(again.attempts.last().unwrap().to_seq, Some(end));
    }

    #[tokio::test]
    async fn restart_finishes_every_pending_attempt_after_the_first_change() {
        use crate::store::AttemptOutcome;
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        engine.0.begin_attempt(&child).unwrap();
        let id = {
            let mut st = child.st.lock();
            let pending = st.internal.attempts[0].clone();
            for n in 1..3 {
                let mut a = pending.clone();
                a.id = format!("restart-pending-{n}");
                st.internal.attempts.push(a);
            }
            let mut done = pending;
            done.id = "already-terminal".into();
            assert!(done.finish(AttemptOutcome::Completed, 7, None));
            st.internal.attempts.push(done);
            engine.0.store.put_session(&st.info, &st.internal).unwrap();
            st.info.id.clone()
        };
        engine.0.mark_interrupted(false).unwrap();
        let (_, saved) = engine.0.store.get_session(&id).unwrap().unwrap();
        assert_eq!(saved.attempts.len(), 4);
        for a in &saved.attempts[..3] {
            assert_eq!(a.outcome, Some(AttemptOutcome::Interrupted));
            assert!(a.to_seq.is_some());
        }
        assert_eq!(saved.attempts[3].outcome, Some(AttemptOutcome::Completed));
        assert_eq!(saved.attempts[3].to_seq, Some(7));
    }

    #[tokio::test]
    async fn watchdog_persists_timed_out_attempt_boundary_and_readback() {
        use crate::store::AttemptOutcome;
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let plan = engine
            .0
            .plan_tasks(
                &parent,
                vec![SubTask {
                    name: "timeout fixture".into(),
                    prompt: "test".into(),
                    ..Default::default()
                }],
            )
            .unwrap()
            .remove(0);
        let child = engine
            .0
            .start_child(&parent, "timeout-call", &plan)
            .unwrap();
        let attempt_id = engine.0.begin_attempt(&child);
        engine
            .0
            .append(&child, said(0, "Older success.").body)
            .unwrap();
        engine
            .0
            .end_attempt(&child, attempt_id.as_deref(), &super::super::Outcome::Done);
        engine
            .0
            .append(
                &child,
                EntryBody::User {
                    content: vec![ContentPart::text("Continue")],
                    author: Some("parent".into()),
                },
            )
            .unwrap();
        let attempt_id = engine.0.begin_attempt(&child);
        let mut partial = said(0, "Timed-out current notes.");
        if let EntryBody::Assistant { stop, .. } = &mut partial.body {
            *stop = StopReason::Error;
        }
        engine.0.append(&child, partial.body).unwrap();
        let cancel = CancellationToken::new();
        engine
            .0
            .watch_attempt(child.clone(), cancel.clone(), Some(Instant::now()))
            .await
            .unwrap();
        assert!(cancel.is_cancelled());
        engine.0.end_attempt(
            &child,
            attempt_id.as_deref(),
            &super::super::Outcome::Aborted,
        );
        let id = child.st.lock().info.id.clone();
        let (_, persisted) = engine.0.store.get_session(&id).unwrap().unwrap();
        let attempt = persisted.attempts.last().unwrap();
        assert_eq!(attempt.outcome, Some(AttemptOutcome::TimedOut));
        assert!(attempt.to_seq.unwrap() > attempt.from_seq);
        let parent_id = parent.st.lock().info.id.clone();
        let text = engine.0.subagent_report(&parent_id, &id).unwrap_err();
        assert!(text.starts_with("This attempt timed out"), "{text}");
        assert!(text.contains("Timed-out current notes."), "{text}");
        assert!(text.contains("Earlier report (historical)"), "{text}");
        assert!(
            !text
                .split("## Earlier")
                .next()
                .unwrap()
                .contains("Older success.")
        );
        let result = engine.0.child_result(&child, &cancel, 0);
        assert!(!result.1);
        assert!(result.0.unwrap_err().starts_with("This attempt timed out"));
    }

    #[test]
    fn preflight_hints_cover_writes_shell_and_remote_reads_without_granting_tools() {
        let read = vec!["read".into(), "grep".into()];
        assert_eq!(
            capability_warnings(
                "Save a report.md file, run sha256 on remote Desktop",
                &read,
                true
            )
            .len(),
            3
        );
        assert!(capability_warnings("Find the parser and return findings", &read, true).is_empty());
        let equipped = vec![
            "read".into(),
            "write".into(),
            "bash".into(),
            "machines".into(),
        ];
        assert!(
            capability_warnings(
                "Save report.md and run cargo test on machine Desktop",
                &equipped,
                false
            )
            .is_empty()
        );
        assert_eq!(
            capability_warnings("Run a command on the machine", &equipped, true).len(),
            1
        );
    }

    #[tokio::test]
    async fn queued_helper_manifest_uses_effective_role_and_readonly_tools() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        for (role, readonly, shell) in [
            (Some("research"), false, false),
            (Some("review"), false, true),
            (Some("review"), true, false),
            (None, true, false),
        ] {
            let plan = engine
                .0
                .plan_tasks(
                    &parent,
                    vec![SubTask {
                        name: "capabilities".into(),
                        prompt: "Save report.md and run sha256 on a remote machine".into(),
                        agent: role.map(str::to_string),
                        readonly,
                        ..Default::default()
                    }],
                )
                .unwrap()
                .remove(0);
            let child = engine.0.start_child(&parent, "manifest", &plan).unwrap();
            let note = engine.0.child_capabilities(&child, Some(&plan.task.prompt));
            assert!(
                note.contains(&format!("role: {}", role.unwrap_or("general"))),
                "{note}"
            );
            let names: Vec<_> = engine
                .0
                .base_tool_specs(&child.st.lock())
                .into_iter()
                .map(|t| t.name)
                .collect();
            assert_eq!(names.iter().any(|t| t == "bash"), shell, "{note}");
            for name in &names {
                assert!(note.contains(name), "{name}: {note}");
            }
            assert!(note.contains("Warning:"), "{note}");
            if readonly {
                assert!(note.contains("Read-only removes bash/job"), "{note}");
            }
            assert!(child.st.lock().internal.subagent.as_ref().unwrap().queued);
            // Role narrowing survives loading the persisted session, not just finish_child().
            let id = child.st.lock().info.id.clone();
            engine.0.sessions.lock().remove(&id);
            let loaded = engine.0.rt(&id).unwrap();
            let after: Vec<_> = engine
                .0
                .base_tool_specs(&loaded.st.lock())
                .into_iter()
                .map(|t| t.name)
                .collect();
            assert_eq!(names, after);
        }
    }

    #[tokio::test]
    async fn status_distinguishes_queue_run_budget_and_restart_with_recovery_advice() {
        let (engine, dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        // This case starts before slot admission, unlike the action fixtures.
        {
            let mut st = child.st.lock();
            st.internal.subagent.as_mut().unwrap().queued = true;
            st.info.helper_queued = true;
            st.budget = None;
        }
        let pid = parent.st.lock().info.id.clone();
        let cid = child.st.lock().info.id.clone();
        engine.0.background.lock().insert(
            pid.clone(),
            vec![BgAgent {
                name: "waiting".into(),
                session_id: cid.clone(),
                started: Instant::now(),
                cancel: CancellationToken::new(),
                batch: 1,
                done: false,
                stopped: false,
            }],
        );
        let status = engine.0.agents_status(&pid);
        assert!(
            status.contains("queued — waiting for a provider lane or helper slot"),
            "{status}"
        );
        assert!(!status.contains("— running"), "{status}");
        {
            let mut st = child.st.lock();
            st.internal.subagent.as_mut().unwrap().queued = false;
            st.info.helper_queued = false;
            st.budget = Some(Budget {
                max_steps: 10,
                base: 4,
                used: 0,
                calls: HashMap::new(),
                soft_deadline: Some(Instant::now() + Duration::from_secs(60)),
                wrapped: false,
                closed: false,
            });
            st.info.steps = 7;
        }
        let status = engine.0.agents_status(&pid);
        assert!(status.contains("— running"), "{status}");
        assert!(status.contains("7/"), "{status}");
        engine.0.save(&child);
        // Reopen only this disposable store; no shutdown/resume or model execution.
        let reopened = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let status = reopened.0.agents_status(&pid);
        assert!(status.contains("interrupted by restart"), "{status}");
        assert!(
            status.contains("agents message") && status.contains(&cid),
            "{status}"
        );
        assert!(
            status.contains("do not blindly repeat side effects"),
            "{status}"
        );
    }

    #[tokio::test]
    async fn parent_contention_keeps_tools_but_a_live_downgrade_explains_the_refusal() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let mut held = parent.st.lock();
        let st = child.st.lock();
        let names: Vec<_> = engine
            .0
            .callable_tools(&st)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.iter().any(|t| t == "bash"));
        assert_eq!(engine.0.effective_permission(&st), held.info.permission);
        // Publishing a real policy change caps the already-live child even
        // while the parent's state remains locked.
        held.info.permission = PermissionMode::ReadOnly;
        engine.0.publish_permission(&held);
        let names: Vec<_> = engine
            .0
            .callable_tools(&st)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(!names.iter().any(|t| t == "bash"));
        assert!(
            engine
                .0
                .unavailable_tool(&st, "not_a_tool", &names)
                .starts_with("Unknown tool")
        );
        assert_eq!(engine.0.effective_permission(&st), PermissionMode::ReadOnly);
        assert!(
            engine
                .0
                .unavailable_tool(&st, "bash", &[])
                .contains("read-only setting or current parent/bot policy")
        );
    }

    #[tokio::test]
    async fn a_slow_own_helper_never_hides_independent_work_or_a_finished_report() {
        use crate::tools::meta::completion_state;
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let (child, out) = completed_report(&engine, &parent);
        let pid = parent.st.lock().info.id.clone();
        let cid = child.st.lock().info.id.clone();
        engine
            .0
            .background
            .lock()
            .get_mut(&pid)
            .unwrap()
            .push(BgAgent {
                name: "unrelated slow helper".into(),
                session_id: "slow-fixture".into(),
                started: Instant::now(),
                cancel: CancellationToken::new(),
                batch: 1,
                done: false,
                stopped: false,
            });
        let ledger = TaskLedger {
            plan: vec![PlanItem {
                text: "Wait for unavailable device".into(),
                status: PlanStatus::Blocked,
            }],
            done_criteria: vec!["Fix local tests".into()],
            criteria: vec![CriterionState {
                status: CriterionStatus::Unmet,
                evidence: "Fixable now".into(),
            }],
            ..Default::default()
        };
        parent.st.lock().info.task = Some(ledger);
        assert!(
            engine
                .0
                .completion_check(&parent)
                .unwrap()
                .contains("Fix local tests")
        );
        {
            let mut st = parent.st.lock();
            st.info.task.as_mut().unwrap().plan = vec![
                PlanItem {
                    text: "slow helper work".into(),
                    status: PlanStatus::InProgress,
                },
                PlanItem {
                    text: "finished helper report needs action".into(),
                    status: PlanStatus::InProgress,
                },
            ];
        }
        assert!(
            engine
                .0
                .completion_check(&parent)
                .unwrap()
                .contains("finished helper report needs action")
        );
        engine.0.append(&parent, said(0, "✓").body).unwrap();
        assert!(engine.0.completion_check(&parent).is_none());
        let before = completion_state(parent.st.lock().info.task.as_ref().unwrap());
        parent.st.lock().running = true; // Deterministic queued-report admission, no model request.
        engine.0.deliver_report(&pid, &cid, "finished helper", out);
        {
            let st = parent.st.lock();
            assert_eq!(completion_state(st.info.task.as_ref().unwrap()), before);
            assert!(st.internal.completion_acknowledged.is_none());
            assert!(st.internal.completion_pending.is_none());
            assert_eq!(st.steer.len(), 1);
        }
        assert!(engine.0.background.lock()[&pid].iter().any(|a| !a.done));
        // A stale in-flight mark must not acknowledge the new result generation.
        engine.0.append(&parent, said(0, "✓").body).unwrap();
        assert!(parent.st.lock().internal.completion_acknowledged.is_none());
        assert!(engine.0.completion_check(&parent).is_some());
        engine.0.append(&parent, said(0, "✓").body).unwrap();
        let replay_stops = child.st.lock().turn_stops.clone();
        engine.0.deliver_report(
            &pid,
            &cid,
            "duplicate finished helper",
            ChildOutcome {
                result: Ok("duplicate report".into()),
                note: None,
                stops: replay_stops,
            },
        );
        assert_eq!(parent.st.lock().steer.len(), 1);
        assert!(
            engine.0.completion_check(&parent).is_none(),
            "duplicate source result preserves the new acknowledgement"
        );
        parent.st.lock().running = false;
    }

    #[tokio::test]
    async fn an_idle_parent_loses_its_ack_when_its_child_is_interrupted_by_restart() {
        let (engine, dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let pid = parent.st.lock().info.id.clone();
        let mut info = parent.st.lock().info.clone();
        info.status = SessionStatus::Idle;
        let mut internal = parent.st.lock().internal.clone();
        internal.completion_acknowledged = Some("old reasons".into());
        internal.completion_pending = Some("old in-flight mark".into());
        engine.0.store.put_session(&info, &internal).unwrap();
        let mut child_info = child.st.lock().info.clone();
        child_info.status = SessionStatus::Running;
        engine
            .0
            .store
            .put_session(&child_info, &child.st.lock().internal)
            .unwrap();
        let reopened = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        let (info, internal) = reopened.0.store.get_session(&pid).unwrap().unwrap();
        assert_eq!(info.status, SessionStatus::Idle);
        assert!(
            internal.completion_acknowledged.is_none() && internal.completion_pending.is_none()
        );
    }

    fn completed_report(engine: &Engine, parent: &Rt) -> (Arc<Rt>, ChildOutcome) {
        let child = engine.0.start_lock_order_test_child(parent);
        let stops = child.st.lock().next_turn_stops.clone().unwrap();
        child.st.lock().turn_stops = stops.clone();
        let since = child.st.lock().info.last_seq;
        let attempt_id = engine.0.begin_attempt(&child);
        engine
            .0
            .append(&child, said(0, "completed under the old Stop epoch").body)
            .unwrap();
        engine
            .0
            .end_attempt(&child, attempt_id.as_deref(), &super::super::Outcome::Done);
        let (result, finished) = engine
            .0
            .child_result(&child, &CancellationToken::new(), since);
        assert!(finished);
        let parent_id = parent.st.lock().info.id.clone();
        let child_id = child.st.lock().info.id.clone();
        engine.0.background.lock().insert(
            parent_id,
            vec![BgAgent {
                name: "delayed fixture".into(),
                session_id: child_id,
                started: Instant::now(),
                cancel: CancellationToken::new(),
                batch: 1,
                done: false,
                stopped: false,
            }],
        );
        (
            child,
            ChildOutcome {
                result,
                note: None,
                stops,
            },
        )
    }

    #[tokio::test]
    async fn delayed_finished_report_keeps_helper_marks_during_new_parent_turn() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let (child, out) = completed_report(&engine, &parent);
        let old = out.stops.clone();
        let parent_id = parent.st.lock().info.id.clone();
        let child_id = child.st.lock().info.id.clone();
        // Model completion has happened. Hold delivery across the same gap
        // as collecting/removing a worktree, without sleeps or live models.
        let finished = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let deliver = {
            let me = engine.0.clone();
            let finished = finished.clone();
            let release = release.clone();
            let parent_id = parent_id.clone();
            std::thread::spawn(move || {
                finished.wait();
                release.wait();
                me.deliver_report(&parent_id, &child_id, "delayed fixture", out);
            })
        };
        finished.wait();
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let fresh = engine.0.stops_here();
        assert_ne!(old, fresh);
        engine
            .0
            .send(
                &parent_id,
                vec![ContentPart::text("fresh user message")],
                Delivery::Auto,
                vec![],
            )
            .unwrap();
        let first = {
            let mut st = parent.st.lock();
            assert!(st.running);
            // Begin that admitted turn as run() does, without polling a model.
            st.turn_stops = st.next_turn_stops.take().unwrap();
            assert_eq!(st.turn_stops, fresh);
            st.cancel = Some(CancellationToken::new());
            st.delivering.iter().map(|(_, w)| w.clone()).collect()
        };
        engine.0.open_turn(&parent, first);
        release.wait();
        deliver.join().unwrap(); // No await: no model or Stop watcher was polled.
        let here = engine.0.here_now(); // Snapshot before the parent state lock.
        let mut st = parent.st.lock();
        assert_eq!(st.steer.len(), 1);
        assert_eq!(st.steer[0].stops, old);
        assert!(
            st.steer[0]
                .author
                .as_deref()
                .unwrap()
                .starts_with("subagent:")
        );
        assert!(!st.has_admitted(Queue::Steer));
        assert!(st.take_one(Queue::Steer).is_none());
        assert!(st.take_queue(Queue::Steer).is_empty());
        assert!(
            st.finish_admission(&Outcome::Done, SessionStatus::Idle, &here)
                .is_empty()
        );
        assert!(st.next_turn_stops.is_none());
        assert_eq!(st.steer.len(), 1, "stale report is retained, not consumed");
        assert_eq!(st.steer[0].stops, old);
    }

    #[tokio::test]
    async fn idle_report_uses_outcome_marks_not_a_continued_childs_marks() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let (child, out) = completed_report(&engine, &parent);
        let old = out.stops.clone();
        let parent_id = parent.st.lock().info.id.clone();
        let child_id = child.st.lock().info.id.clone();
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        let fresh = engine.0.stops_here();
        assert_ne!(old, fresh);
        child.st.lock().turn_stops = fresh; // Same runtime has since been continued.
        engine
            .0
            .deliver_report(&parent_id, &child_id, "delayed fixture", out);
        let st = parent.st.lock();
        assert!(!st.running);
        assert!(st.next_turn_stops.is_none());
        let entries = engine.0.store.entries(&parent_id, 0).unwrap();
        assert!(entries.iter().any(|e| matches!(&e.body, EntryBody::User { author: Some(a), .. } if a == &format!("subagent:{child_id}"))));
    }

    #[tokio::test]
    async fn helper_error_outcome_captures_its_origin_before_report_delivery() {
        let (engine, _dir, parent) = super::super::test_support::engine().await;
        let project = tempfile::tempdir().unwrap();
        parent.st.lock().info.cwd = project.path().display().to_string();
        let child = engine.0.start_lock_order_test_child(&parent);
        let old = child.st.lock().next_turn_stops.clone().unwrap();
        crate::node::StopLatch::stop(&engine.0.paths, 0).unwrap();
        crate::node::StopLatch::set(&engine.0.paths, false, 0).unwrap();
        assert_ne!(old, engine.0.stops_here());
        // Finish without a model/provider request: stopped before its turn.
        let stopped = CancellationToken::new();
        stopped.cancel();
        let out = engine.0.clone().finish_child(child, stopped).await;
        assert!(out.result.is_err());
        assert_eq!(out.stops, old);
    }

    fn call(seq: u64, id: &str, name: &str, args: Value) -> Entry {
        Entry {
            seq,
            ts: 0,
            body: EntryBody::Assistant {
                content: vec![AssistantPart::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: args,
                }],
                model: ModelRef {
                    provider: "p".into(),
                    model: "m".into(),
                },
                usage: Usage::default(),
                stop: StopReason::ToolUse,
                error: None,
                author: None,
            },
        }
    }

    fn said(seq: u64, text: &str) -> Entry {
        Entry {
            seq,
            ts: 0,
            body: EntryBody::Assistant {
                content: vec![AssistantPart::Text { text: text.into() }],
                model: ModelRef {
                    provider: "p".into(),
                    model: "m".into(),
                },
                usage: Usage::default(),
                stop: StopReason::Stop,
                error: None,
                author: None,
            },
        }
    }

    #[test]
    fn the_report_is_the_answer_not_the_completion_check_mark() {
        let mut marked = said(4, "Internal acknowledged response");
        if let EntryBody::Assistant { author, .. } = &mut marked.body {
            *author = Some(crate::tools::meta::CHECK_AUTHOR.into());
        }
        let entries = [
            said(1, "Found it: src/a.rs:12."),
            call(2, "t1", "task", serde_json::json!({"update": []})),
            said(3, "✓"),
            marked,
        ];
        assert_eq!(
            last_answer(&entries).as_deref(),
            Some("Found it: src/a.rs:12.")
        );
    }

    fn done(seq: u64, id: &str, exit: i64) -> Entry {
        Entry {
            seq,
            ts: 0,
            body: EntryBody::ToolResult {
                call_id: id.into(),
                tool: "bash".into(),
                content: vec![ContentPart::text("…")],
                is_error: exit != 0,
                artifact: None,
                details: Some(json!({ "exit_code": exit })),
            },
        }
    }

    #[test]
    fn a_stopped_subagent_still_says_what_it_did() {
        let entries = vec![
            call(
                1,
                "a",
                "write",
                json!({"path": "/w/env.sh", "content": "x"}),
            ),
            call(2, "b", "bash", json!({"command": "./build.sh"})),
            done(3, "b", 0),
            call(4, "c", "bash", json!({"command": "./smoke-test.sh"})),
            done(5, "c", 1),
            call(6, "d", "bash", json!({"command": "sleep 100"})),
        ];
        let d = activity_digest(&entries).unwrap();
        assert!(d.contains("(4 tool calls)"), "{d}");
        assert!(d.contains("Changed: /w/env.sh"), "{d}");
        assert!(
            d.contains("`./build.sh` (ok); `./smoke-test.sh` (failed); `sleep 100` (no result)"),
            "{d}"
        );
        assert_eq!(activity_digest(&[]), None);
        assert_eq!(
            join_digest("stopped before it wrote a report".into(), Some(d.clone())),
            format!("stopped before it wrote a report.\n\n{d}")
        );
    }
}
