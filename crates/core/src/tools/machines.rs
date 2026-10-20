//! `machines`: the user's other computers that run `tessera node` (a desktop
//! with a fast CPU, a laptop…). List them, run commands there, read and
//! change files there, within what each machine allows.
//!
//! Every machine decides for itself (its scope is set only on that
//! computer and checked there on every action); this tool only checks the
//! reported scope first so a refusal comes before an approval, checks who
//! may use the machine (any chat, or only certain bots: the session's
//! acting bot, which subagents and scheduled duties carry) and how far that
//! bot may go there (its own access: none, read only, or as the machine
//! allows), and asks the user as the machine's "Ask first" choice and the
//! chat's mode say (the stricter wins). Approval cards name the machine:
//! "on Desktop: cargo build".

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{
    AdminAccess, ContentPart, MachineAccess, MachineAsk, MachineInfo, MachinePreset, NodeAction,
    NodeResult, PermissionMode,
};
use tokio_util::sync::CancellationToken;

use super::{
    Approve, Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_bool, opt_str, opt_u64,
};
use crate::auth::{SecretPlace, SecretUse};

/// One action for a machine, as the engine sends it.
pub struct MachineCall {
    pub session_id: String,
    /// The machine's id.
    pub machine: String,
    pub action: NodeAction,
    /// The user approved it in the chat.
    pub approved: bool,
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
    pub cancel: CancellationToken,
    /// Stored secrets a `run` gets there (only what this chat may use on
    /// this machine).
    pub env: tessera_proto::SecretEnv,
    /// The machine's count of its user's Stops when this was asked for.
    pub stops: Option<u32>,
}

const ACTIONS: &[&str] = &["list", "run", "read", "ls", "write", "edit"];

pub struct Machines;

impl Tool for Machines {
    fn name(&self) -> &'static str {
        "machines"
    }
    fn description(&self) -> String {
        "The user's other computers turned on for Tessera (\"machines\"), e.g. a fast desktop for builds. `list` shows the \
         ones this chat may use, what each allows (folders, commands, administrator) and when it asks first. `run` a command \
         there (`cwd` in an allowed folder; `admin: true`: with sudo working, where allowed), `read`/`ls`, `write`, `edit` \
         (exact `old` → `new`). Paths are that machine's own (absolute or ~/). Commands can't add entries to the home folder \
         itself (use `write`). Each machine sets what it allows on that computer; nothing here widens it. `docs machines` \
         explains more."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ACTIONS},
                "machine": {"type": "string", "description": "Name or id (optional when there is only one)"},
                "command": {"type": "string"},
                "cwd": {"type": "string", "description": "Working folder for run (default ~); must be allowed there"},
                "admin": {"type": "boolean", "description": "As administrator (sudo works); only where the machine allows it"},
                "timeout": {"type": "integer", "description": "Seconds (capped by the machine's limit)"},
                "path": {"type": "string"},
                "offset": {"type": "integer"},
                "limit": {"type": "integer"},
                "content": {"type": "string"},
                "old": {"type": "string"},
                "new": {"type": "string"},
                "all": {"type": "boolean"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        // Reads and changes in one tool: the permission mode is applied per
        // action below (read-only chats still list and read).
        ToolKind::Meta
    }
    fn concurrent(&self, args: &Value) -> bool {
        matches!(opt_str(args, "action"), Some("list" | "read" | "ls"))
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("list");
        let machine = opt_str(args, "machine").unwrap_or("machine");
        match opt_str(args, "command").or(opt_str(args, "path")) {
            Some(t) if action != "list" => {
                format!("on {machine}: {}", crate::util::one_line(t, 100))
            }
            _ => action.to_string(),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let action = opt_str(args, "action").unwrap_or("");
        let changes = matches!(action, "run" | "write" | "edit");
        if changes && ctx.host.permission(&ctx.session_id) == PermissionMode::ReadOnly {
            return Risk::Block {
                reason: "This chat is read-only: machines can be listed and read, not changed."
                    .into(),
            };
        }
        if action == "run"
            && let Some(cmd) = opt_str(args, "command")
            && let block @ Risk::Block { .. } = super::policy::assess_command(cmd)
        {
            return block;
        }
        Risk::None
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match run(&args, ctx).await {
                Ok(out) => out,
                Err(e) => e,
            }
        })
    }
}

/// The machines this chat may use (`read_only`: the ids where its bot may
/// only read), and how many more it may not.
fn list(machines: &[MachineInfo], read_only: &[String], hidden: usize) -> String {
    if machines.is_empty() {
        if hidden > 0 {
            return format!(
                "No machine this chat may use ({hidden} not open to it). Who may use a machine is set on that computer; a bot's own access is in its settings (Machines)."
            );
        }
        return "No machines yet. A computer becomes one when the user turns it on there: \
                `tessera node enable` in its terminal, or Settings → Devices in its desktop app \
                (see `docs machines`)."
            .into();
    }
    let mut out = String::new();
    for m in machines {
        let state = if !m.online {
            format!("offline (last seen {})", crate::jobs::fmt_time(m.last_seen))
        } else if m.paused {
            "online, paused".into()
        } else if m.running > 0 {
            format!("online, {} action(s) running", m.running)
        } else {
            "online".into()
        };
        out.push_str(&format!("{} ({}) · {} · {state}\n", m.name, m.os, m.id));
        if read_only.contains(&m.id) {
            out.push_str(
                "  this bot may only read here (files and folders); no commands or changes\n",
            );
        }
        if m.preset != MachinePreset::Custom {
            out.push_str(&format!(
                "  {}: {}\n",
                crate::node::preset_name(m.preset),
                crate::node::preset_sentence_with_sandbox(
                    m.preset,
                    &m.os,
                    m.scope.commands && m.sandbox_ok
                )
            ));
        }
        if let Some(h) = &m.home {
            out.push_str(&format!("  home: {h} (~)\n"));
        }
        if m.scope.folders.is_empty() {
            out.push_str("  folders: none allowed yet\n");
        }
        for f in &m.scope.folders {
            out.push_str(&format!(
                "  {} {}\n",
                if f.write { "read-write" } else { "read-only " },
                f.path
            ));
        }
        let commands = match (m.scope.commands, m.sandbox_ok) {
            (false, _) => "off".to_string(),
            (true, true) => "on".to_string(),
            (true, false) => format!("on, but refused there (sandbox {})", m.sandbox),
        };
        out.push_str(&format!(
            "  commands: {commands}{} · asks first: {} · limit {} per action\n",
            if m.scope.desktop {
                " with the desktop session (desktop control: startup files may change)"
            } else {
                ""
            },
            match m.scope.ask {
                MachineAsk::Always => "every action",
                MachineAsk::Risky => "risky actions",
                MachineAsk::Chat => "as this chat's mode says",
            },
            crate::node::human_secs(m.scope.timeout_secs)
        ));
        if m.scope.webviews {
            out.push_str("  web views can run here\n");
        }
        match m.scope.admin {
            AdminAccess::Off => out.push_str("  administrator: off — no Tessera elevation; admin:true is refused. Approval or a saved password cannot enable it; only the user can change this on that computer\n"),
            AdminAccess::Ask => out.push_str(
                "  administrator: on (`run` with `admin: true`; each one asks the user first)\n",
            ),
            AdminAccess::Chat => out.push_str(
                "  administrator: on (`run` with `admin: true`; asks as this chat's mode says)\n",
            ),
        }
    }
    if hidden > 0 {
        out.push_str(&format!("({hidden} more not open to this chat.)\n"));
    }
    out.trim_end().to_string()
}

/// The machine `args` names (or the only one).
fn pick<'a>(machines: &'a [MachineInfo], args: &Value) -> Result<&'a MachineInfo, ToolOutput> {
    let names = || {
        machines
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match opt_str(args, "machine").map(str::trim) {
        Some(want) => machines
            .iter()
            .find(|m| m.id == want)
            .or_else(|| machines.iter().find(|m| m.name.eq_ignore_ascii_case(want)))
            .ok_or_else(|| {
                ToolOutput::error(format!(
                    "No machine `{want}`. Machines: {}.",
                    if machines.is_empty() {
                        "none".into()
                    } else {
                        names()
                    }
                ))
            }),
        None if machines.len() == 1 => Ok(&machines[0]),
        None if machines.is_empty() => Err(ToolOutput::error(list(machines, &[], 0))),
        None => Err(ToolOutput::error(format!(
            "Say which machine: {}.",
            names()
        ))),
    }
}

fn action_of(action: &str, args: &Value) -> Result<NodeAction, ToolOutput> {
    let s = |k: &str| arg_str(args, k).map(str::to_string);
    Ok(match action {
        "run" => NodeAction::Run {
            command: s("command")?,
            cwd: opt_str(args, "cwd").unwrap_or("~").to_string(),
            timeout_secs: opt_u64(args, "timeout"),
            admin: opt_bool(args, "admin").unwrap_or(false),
        },
        "read" => NodeAction::Read {
            path: s("path")?,
            offset: opt_u64(args, "offset"),
            limit: opt_u64(args, "limit"),
        },
        "ls" => NodeAction::List { path: s("path")? },
        "write" => NodeAction::Write {
            path: s("path")?,
            content: args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolOutput::error("Missing required string argument `content`."))?
                .to_string(),
        },
        "edit" => NodeAction::Edit {
            path: s("path")?,
            old: s("old")?,
            new: args
                .get("new")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolOutput::error("Missing required string argument `new`."))?
                .to_string(),
            all: opt_bool(args, "all").unwrap_or(false),
        },
        other => {
            return Err(ToolOutput::error(format!(
                "Unknown action `{other}`; one of: {}.",
                ACTIONS.join(", ")
            )));
        }
    })
}

/// `path` lies in `folder` (lexically; `/` separated, as reported).
fn inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    path == folder || path.starts_with(&format!("{folder}/"))
}

/// The reported scope's verdict, so plain refusals come before asking the
/// user (the machine checks again, for real). Paths with `~` or `..` are
/// left to the machine.
fn precheck(m: &MachineInfo, action: &NodeAction) -> Result<(), String> {
    if !m.online {
        return Err(format!("{} is offline right now.", m.name));
    }
    if m.paused {
        return Err(format!(
            "{} is paused; the user can resume it in Settings → Devices.",
            m.name
        ));
    }
    if m.scope.stopped {
        return Err(format!(
            "The user stopped Tessera on {} (its Stop bar or Stop key): it does nothing until they resume it on that computer. Don't retry; tell the user.",
            m.name
        ));
    }
    let target = action.target();
    let (path, write) = match action {
        NodeAction::Desktop { .. } => return desktop_precheck(m),
        NodeAction::Run { cwd, admin, .. } => {
            if *admin && m.scope.admin == AdminAccess::Off {
                return Err(format!(
                    "Administrator commands are off on {0}. Only the user can turn them on, on that computer (Settings → Devices → This computer → Administrator, or `tessera node admin on`). Otherwise give the user the exact command to run there.",
                    m.name
                ));
            }
            if !m.scope.commands {
                return Err(format!(
                    "Commands are off on {}. Only the user can turn them on, on that computer (Work in folders or Full access: `tessera node preset`, or Settings → Devices in its desktop app).",
                    m.name
                ));
            }
            if !m.sandbox_ok {
                return Err(format!(
                    "{} can't run commands: they need its sandbox, which is {}.",
                    m.name, m.sandbox
                ));
            }
            (cwd.as_str(), false)
        }
        a => (target.as_ref(), a.changes()),
    };
    if !path.starts_with('/') || path.split('/').any(|c| c == "..") {
        return Ok(());
    }
    let rule = m
        .scope
        .folders
        .iter()
        .filter(|f| inside(path, &f.path))
        .max_by_key(|f| f.path.len());
    match rule {
        None => Err(format!(
            "{path} is outside the folders {} allows ({}). Only the user can allow more, on that computer.",
            m.name,
            if m.scope.folders.is_empty() {
                "none yet".to_string()
            } else {
                m.scope
                    .folders
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
        Some(f) if write && !f.write => Err(format!(
            "{} is read-only on {}. Only the user can make it read-write, on that computer.",
            f.path, m.name
        )),
        _ => Ok(()),
    }
}

/// A desktop action as the approval shows it: what gets typed exactly,
/// with Enter and Tab visible (they can send a message or run a command).
fn desktop_approval(op: &tessera_proto::DesktopOp) -> String {
    match op {
        tessera_proto::DesktopOp::Type { text } => {
            let shown: String = text
                .replace("\r\n", "\n")
                .chars()
                .map(|c| match c {
                    '\n' | '\r' => "⏎".to_string(),
                    '\t' => "⇥".to_string(),
                    c => c.to_string(),
                })
                .collect();
            // All of it (typing takes at most 5,000 characters): what is
            // approved is exactly what gets typed.
            format!("type “{shown}”")
        }
        other => other.describe(),
    }
}

/// Plain reasons a desktop action can't happen there, before anyone is asked.
fn desktop_precheck(m: &MachineInfo) -> Result<(), String> {
    if !m.features.iter().flatten().any(|f| f == "desktop") {
        return Err(if m.os.starts_with("windows") {
            format!(
                "Tessera on {0} is older than desktop actions: the user can update it there (the newest Windows installer), then turn on desktop control.",
                m.name
            )
        } else {
            format!(
                "Desktop actions aren't available on {} ({}) yet. With desktop control on there, its commands get the desktop session and can use the desktop's own tools (for example grim, ydotool, hyprctl).",
                m.name, m.os
            )
        });
    }
    if !m.scope.desktop {
        return Err(format!(
            "Desktop control is off on {0}. Only the user can turn it on, on that computer (Settings → Devices → This computer → Desktop control, or `tessera node preset desktop`).",
            m.name
        ));
    }
    Ok(())
}

async fn run(args: &Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolOutput> {
    let action = opt_str(args, "action").unwrap_or("list");
    if !ACTIONS.contains(&action) {
        return Err(ToolOutput::error(format!(
            "Unknown action `{action}`; one of: {}.",
            ACTIONS.join(", ")
        )));
    }
    let node_action = (action != "list")
        .then(|| action_of(action, args))
        .transpose()?;
    act(args, ctx, node_action).await
}

/// `node_action` on the machine `args` names, asked and refused as that
/// machine and this chat say (`None`: list the machines). The `desktop`
/// tool's actions come here too.
pub(super) async fn act(
    args: &Value,
    ctx: &ToolCtx,
    node_action: Option<NodeAction>,
) -> Result<ToolOutput, ToolOutput> {
    // Only the machines this chat may use (its bot, if any, decides), and
    // the ones where that bot may only read.
    let all = ctx.host.machines();
    let total = all.len();
    let named = opt_str(args, "machine").map(str::trim).map(str::to_string);
    let mut refused_here: Option<(MachineInfo, String)> = None;
    let mut read_only: Vec<String> = Vec::new();
    let machines: Vec<MachineInfo> = all
        .into_iter()
        .filter(|m| match ctx.host.machine_access(&ctx.session_id, m) {
            Ok(level) => {
                if level == MachineAccess::ReadOnly {
                    read_only.push(m.id.clone());
                }
                true
            }
            Err(why) => {
                if named
                    .as_deref()
                    .is_some_and(|n| m.id == n || m.name.eq_ignore_ascii_case(n))
                {
                    refused_here = Some((m.clone(), why));
                }
                false
            }
        })
        .collect();
    let Some(node_action) = node_action else {
        return Ok(ToolOutput::text(list(
            &machines,
            &read_only,
            total - machines.len(),
        )));
    };
    if let Some((m, why)) = refused_here {
        let why = format!("{}: {why}; continue without it.", m.name);
        ctx.host
            .machine_refused(&ctx.session_id, &m.id, node_action, &why);
        return Err(ToolOutput::error(why));
    }
    if machines.is_empty() && total > 0 {
        return Err(ToolOutput::error(list(&machines, &[], total)));
    }
    let m = pick(&machines, args)?;
    let mode = ctx.host.permission(&ctx.session_id);
    // Refused here: the machine still hears of it, for its own log.
    let refuse = |why: String| {
        ctx.host
            .machine_refused(&ctx.session_id, &m.id, node_action.clone(), &why);
        let outcome = crate::approval::Outcome::Rule(why);
        crate::approval::audit(
            &ctx.session_id,
            "machines",
            &crate::approval::Answer::rule(outcome.clone()),
        );
        ToolOutput::error(outcome.message("machines"))
    };
    if mode == PermissionMode::ReadOnly && node_action.changes() {
        return Err(refuse(
            "This chat is read-only: machines can be listed and read, not changed. The user can change the chat's mode.".into(),
        ));
    }
    // The bot may only read files there: refused before anyone is asked
    // (looking at the screen is more than reading files).
    if read_only.contains(&m.id) && !node_action.only_reads_files() {
        return Err(refuse(format!(
            "{}: this bot may only read files and list folders there; its screen, commands and changes need more access, which the user can give in the bot's settings (Machines).",
            m.name
        )));
    }
    precheck(m, &node_action).map_err(&refuse)?;
    // Stored secrets the command names: only those this chat may use on
    // this machine reach it (who and where: engine/security.rs).
    let place = SecretPlace::Machine(&m.id);
    let secrets = match &node_action {
        NodeAction::Run { command, .. } => ctx.host.secret_env(&ctx.session_id, command, place),
        _ => SecretUse::default(),
    };
    if let Some(why) = super::secrets::elsewhere(&secrets, || ctx.host.machines(), place) {
        return Err(refuse(why));
    }
    let secret_asks: Vec<String> = secrets
        .used
        .iter()
        .filter(|(_, ask, _)| *ask)
        .map(|(name, _, hosts)| {
            if hosts.is_empty() {
                format!("${name}")
            } else {
                format!("${name} (for {})", hosts.join(", "))
            }
        })
        .collect();
    let secret_reason = (!secret_asks.is_empty()).then(|| {
        format!(
            "This command uses {} from your passwords & keys. The program can send the value anywhere; listed sites are not a network boundary.",
            secret_asks.join(", ")
        )
    });

    // Who decides: the machine's "Ask first" choice (every action by
    // default), and the chat's mode as for this server's own tools; the
    // stricter wins.
    let policy = match &node_action {
        NodeAction::Run { command, .. } => {
            let risk = super::policy::assess_command_for(command, m.os.starts_with("windows"));
            // No server-side path resolution can authorize a different
            // computer's read operands. Require its ordinary command approval.
            if matches!(risk, Risk::None) && super::policy::mentions_read_program(command) {
                Risk::Ask {
                    reason:
                        "The command reads files on another computer; its targets need approval."
                            .into(),
                    key: "bash:read-targets".into(),
                }
            } else {
                risk
            }
        }
        _ => Risk::None,
    };
    if let Risk::Block { reason } = &policy {
        return Err(refuse(reason.clone()));
    }
    let home = m.home.as_deref().map(std::path::Path::new);
    let risky = crate::node::risky_on(
        &node_action,
        |p| m.scope.desktop && crate::node::may_be_startup(p, home),
        m.os.starts_with("windows"),
    );
    // As administrator, by default each command asks, whatever the chat's mode.
    let admin = matches!(node_action, NodeAction::Run { admin: true, .. });
    let admin_asks = admin && m.scope.admin == AdminAccess::Ask;
    let machine_asks = admin_asks
        || match m.scope.ask {
            MachineAsk::Always => true,
            MachineAsk::Risky => risky.is_some() || matches!(policy, Risk::Ask { .. }),
            MachineAsk::Chat => false,
        };
    // After the user pressed Stop there, even a YOLO chat asks once (with
    // "allow for this chat") before it uses that screen again.
    let rearm = matches!(node_action, NodeAction::Desktop { .. })
        && mode == PermissionMode::Auto
        && m.scope.stops > 0;
    // Asked even in a YOLO chat (the machine's own rule, or a Stop there).
    let always = machine_asks || rearm;
    let mode_asks = match (&node_action, mode) {
        // The screen may show anything private, and a click can do anything:
        // outside YOLO chats the chat asks first (once, with "allow for
        // this chat"), whatever the machine says.
        (NodeAction::Desktop { .. }, PermissionMode::Auto) => rearm,
        (NodeAction::Desktop { .. }, _) => true,
        (_, PermissionMode::Auto | PermissionMode::ReadOnly) => false,
        (_, PermissionMode::Ask) => node_action.changes(),
        (_, PermissionMode::Default) => matches!(policy, Risk::Ask { .. }),
    };
    // A secret any chat may use asks before its first use in this chat,
    // as on this server (chats that run without asking don't).
    let secret_ask = secret_reason.is_some() && mode != PermissionMode::Auto;
    let ask = machine_asks || mode_asks || secret_ask;
    let target = node_action.target().to_string();
    let approved = if ask {
        let mut reason = match &node_action {
            NodeAction::Run { cwd, .. } if admin => format!(
                "Runs a command as administrator (root, with sudo) on {} in {cwd}, outside its sandbox.",
                m.name
            ),
            NodeAction::Run { cwd, .. } => format!("Runs a command on {} in {cwd}.", m.name),
            NodeAction::Read { path, .. } => format!("Reads {path} on {}.", m.name),
            NodeAction::List { path } => format!("Lists {path} on {}.", m.name),
            NodeAction::Write { path, .. } => format!("Writes {path} on {}.", m.name),
            NodeAction::Edit { path, .. } => format!("Edits {path} on {}.", m.name),
            NodeAction::Desktop { desktop, look } if desktop.acts() => format!(
                "Uses the mouse or keyboard on {}'s screen: {}.{}",
                m.name,
                desktop_approval(desktop),
                if *look {
                    " Then shows the agent the screen."
                } else {
                    ""
                }
            ),
            NodeAction::Desktop { desktop, .. } => format!(
                "Looks at {}'s screen{}.",
                m.name,
                match desktop.screen() {
                    n if n > 1 => format!(" (screen {n})"),
                    _ => String::new(),
                }
            ),
        };
        match (&policy, &risky) {
            (Risk::Ask { reason: r, .. }, _) | (_, Some(r)) => {
                reason.push(' ');
                reason.push_str(r);
            }
            _ => {}
        }
        match m.scope.ask {
            _ if admin_asks => reason.push_str(&format!(
                " {} asks before every administrator command.",
                m.name
            )),
            MachineAsk::Always => {
                reason.push_str(&format!(" {} asks before every action.", m.name))
            }
            MachineAsk::Risky if machine_asks => {
                reason.push_str(&format!(" {} asks before risky actions.", m.name))
            }
            _ => {}
        }
        if rearm {
            reason.push_str(&format!(
                " Tessera was stopped on {} earlier, so each chat asks once again before using its screen.",
                m.name
            ));
        }
        if let Some(r) = &secret_reason {
            reason.push(' ');
            reason.push_str(r);
        }
        let mut arguments = args.clone();
        if let Some(o) = arguments.as_object_mut() {
            o.insert("machine".into(), json!(m.name));
            o.insert("machine_id".into(), json!(m.id));
        }
        let key = match &node_action {
            // Only the secrets ask: "allow for this chat" covers them, as
            // for commands on this server.
            _ if !machine_asks && !mode_asks => {
                let names: Vec<&str> = secrets
                    .used
                    .iter()
                    .filter(|(_, ask, _)| *ask)
                    .map(|(n, ..)| n.as_str())
                    .collect();
                names
                    .iter()
                    .map(|n| format!("secret:{n}"))
                    .collect::<Vec<_>>()
                    .join("+")
            }
            NodeAction::Run { cwd, command, .. } if admin => {
                format!("machine:{}:admin:{cwd}:{command}", m.id)
            }
            NodeAction::Run { cwd, command, .. } => format!("machine:{}:run:{cwd}:{command}", m.id),
            // "Allow for this chat" covers using that screen, not one click,
            // and only until the user's next Stop there.
            NodeAction::Desktop { desktop, .. } if desktop.acts() => {
                format!("machine:{}:desktop:{}", m.id, m.scope.stops)
            }
            NodeAction::Desktop { .. } => format!("machine:{}:screen:{}", m.id, m.scope.stops),
            a => format!("machine:{}:{}:{}", m.id, a.name(), a.target()),
        };
        let allowed = ctx
            .host
            .approve_reason(Approve {
                session_id: ctx.session_id.clone(),
                call_id: ctx.call_id.clone(),
                tool: "machines".into(),
                summary: format!("on {}: {}", m.name, crate::util::one_line(&target, 200)),
                reason,
                key,
                always,
                cancel: ctx.cancel.clone(),
                arguments,
                // Only the machine's own rule asks every time.
                once: machine_asks,
            })
            .await;
        if let Err(why) = allowed {
            ctx.host
                .machine_refused(&ctx.session_id, &m.id, node_action.clone(), &why);
            return Err(ToolOutput::error(why));
        }
        true
    } else {
        false
    };

    if !secrets.used.is_empty() {
        let names: Vec<String> = secrets.used.iter().map(|(n, ..)| n.clone()).collect();
        ctx.host.mark_secrets_used(&names);
    }
    let result: NodeResult = ctx
        .host
        .machine_call(MachineCall {
            session_id: ctx.session_id.clone(),
            machine: m.id.clone(),
            action: node_action,
            approved,
            progress: ctx.progress.clone(),
            cancel: ctx.cancel.clone(),
            env: tessera_proto::SecretEnv(secrets.env.into_iter().collect()),
            stops: Some(m.scope.stops),
        })
        .await
        .map_err(ToolOutput::error)?;
    let mut text = if result.refused {
        format!("{} refused this: {}", m.name, result.output)
    } else {
        result.output
    };
    // sudo that couldn't work: what to do instead, in plain words.
    if !result.refused
        && let Some(hint) = crate::node::admin::sudo_hint(&text, admin)
    {
        text.push('\n');
        text.push_str(hint);
        if !admin {
            text.push_str(&if m.scope.admin == AdminAccess::Off {
                format!(
                    " Administrator commands are off on {0}: the user can turn them on there (Settings → Devices → This computer → Administrator). Otherwise give the user the exact command to run on {0}.",
                    m.name
                )
            } else {
                format!(
                    " {} allows administrator commands: run it again with `admin: true`.",
                    m.name
                )
            });
        }
    }
    let (text, artifact) = super::fit_output(
        &text,
        ctx,
        &format!("{}: {}", m.name, crate::util::one_line(&target, 60)),
    );
    let mut details = json!({ "machine": m.name, "machine_id": m.id, "refused": result.refused });
    if let Some(code) = result.exit_code {
        details["exit_code"] = json!(code);
    }
    if let Some(a) = artifact {
        details["artifact"] = json!(a);
    }
    let mut out = ToolOutput {
        is_error: !result.ok,
        ..ToolOutput::text(text)
    };
    // A desktop screenshot, for the model to see.
    if let Some(image) = result.image {
        details["image"] = json!({ "width": image.width, "height": image.height });
        out.content.push(ContentPart::Image {
            mime: image.mime,
            data: image.data,
        });
    }
    Ok(out.with_details(details))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_proto::{FolderAccess, MachineScope};

    #[test]
    fn administrator_listing_is_explicit_even_when_off() {
        let mut m = machine();
        for (access, expected) in [
            (
                AdminAccess::Off,
                "administrator: off — no Tessera elevation",
            ),
            (AdminAccess::Ask, "each one asks the user first"),
            (AdminAccess::Chat, "asks as this chat's mode says"),
        ] {
            m.scope.admin = access;
            let text = list(&[m.clone()], &[], 0);
            assert!(text.contains(expected), "{text}");
        }
        m.scope.admin = AdminAccess::Off;
        let action = action_of(
            "run",
            &json!({"command":"echo hello", "cwd":"/home/u/code", "admin":true}),
        )
        .unwrap();
        assert!(
            precheck(&m, &action)
                .unwrap_err()
                .contains("Administrator commands are off")
        );
    }

    #[test]
    fn optional_working_folder_defaults_home_and_keeps_node_scope_checks() {
        let action = action_of("run", &json!({"command":"pwd"})).unwrap();
        assert!(matches!(action, NodeAction::Run { ref cwd, admin: false, .. } if cwd == "~"));
        assert!(
            !Machines.schema()["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "cwd")
        );
        assert!(
            Machines.schema()["properties"]["cwd"]["description"]
                .as_str()
                .unwrap()
                .contains("default ~")
        );
        assert!(precheck(&machine(), &run_in("/outside")).is_err());
        assert!(action_of("run", &json!({})).is_err());
        assert!(action_of("list", &json!({})).is_err()); // list needs no NodeAction
    }

    fn machine() -> MachineInfo {
        MachineInfo {
            id: "dev1".into(),
            name: "Desktop".into(),
            os: "linux x86_64".into(),
            version: "0.1.0".into(),
            online: true,
            last_seen: 0,
            paused: false,
            scope: MachineScope {
                folders: vec![
                    FolderAccess {
                        path: "/home/u/code".into(),
                        write: true,
                    },
                    FolderAccess {
                        path: "/home/u/docs".into(),
                        write: false,
                    },
                ],
                commands: true,
                ..Default::default()
            },
            sandbox: "on".into(),
            sandbox_ok: true,
            running: 0,
            preset: MachinePreset::Custom,
            home: Some("/home/u".into()),
            recent: Vec::new(),
            features: None,
        }
    }

    fn run_in(cwd: &str) -> NodeAction {
        NodeAction::Run {
            command: "make".into(),
            cwd: cwd.into(),
            timeout_secs: None,
            admin: false,
        }
    }

    #[test]
    fn the_reported_scope_refuses_before_anyone_is_asked() {
        let m = machine();
        assert!(precheck(&m, &run_in("/home/u/code/app")).is_ok());
        assert!(
            precheck(&m, &run_in("/home/u/codex")).is_err(),
            "a prefix isn't a folder"
        );
        assert!(
            precheck(
                &m,
                &NodeAction::Write {
                    path: "/home/u/docs/a".into(),
                    content: String::new()
                }
            )
            .is_err()
        );
        assert!(
            precheck(
                &m,
                &NodeAction::Read {
                    path: "/home/u/docs/a".into(),
                    offset: None,
                    limit: None
                }
            )
            .is_ok()
        );
        // `~` and `..` are for the machine to resolve.
        assert!(
            precheck(
                &m,
                &NodeAction::Read {
                    path: "~/x".into(),
                    offset: None,
                    limit: None
                }
            )
            .is_ok()
        );
        let mut off = m.clone();
        off.scope.commands = false;
        assert!(
            precheck(&off, &run_in("/home/u/code"))
                .unwrap_err()
                .contains("Only the user")
        );
        let mut paused = m.clone();
        paused.paused = true;
        assert!(precheck(&paused, &run_in("/home/u/code")).is_err());
        let mut gone = m;
        gone.online = false;
        assert!(precheck(&gone, &run_in("/home/u/code")).is_err());
    }

    #[test]
    fn machines_are_picked_by_name_or_id() {
        let ms = vec![machine()];
        assert_eq!(pick(&ms, &json!({})).unwrap().id, "dev1");
        assert_eq!(
            pick(&ms, &json!({"machine": "desktop"})).unwrap().id,
            "dev1"
        );
        assert!(pick(&ms, &json!({"machine": "laptop"})).is_err());
        let text = list(&ms, &[], 0);
        assert!(
            text.contains("Desktop") && text.contains("read-write /home/u/code"),
            "{text}"
        );
        assert!(!text.contains("only read here"), "{text}");
        let text = list(&ms, &["dev1".into()], 0);
        assert!(text.contains("this bot may only read here"), "{text}");
        let text = list(&[], &[], 2);
        assert!(text.contains("2 not open to it"), "{text}");
        let text = list(&ms, &[], 1);
        assert!(text.contains("1 more not open to this chat"), "{text}");
    }
}
