//! Outward-facing file selection, separate from ordinary page clicks.
use super::*;
use crate::browser::agent_upload::{self, Sources};

fn local_source(path: &std::path::Path, ctx: &ToolCtx) -> Result<(), String> {
    agent_upload::source_path(
        path,
        &ctx.host.protected_paths(),
        dirs::home_dir().as_deref(),
    )?;
    super::super::files::authorize_file(path, ctx, false)
}

fn machine_rule(tab: &Tab, ctx: &ToolCtx) -> Result<tessera_proto::MachineAsk, String> {
    let Some(id) = tab.machine() else {
        return Ok(tessera_proto::MachineAsk::Chat);
    };
    ctx.host
        .machines()
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| m.scope.ask)
        .ok_or_else(|| "The tab's machine is no longer available.".into())
}

pub(super) async fn upload(tab: &Arc<Tab>, args: &Value, ctx: &ToolCtx) -> Result<String, String> {
    if ctx.host.permission(&ctx.session_id) == PermissionMode::ReadOnly {
        return Err("This chat is read-only and cannot upload files.".into());
    }
    let paths: Vec<String> = serde_json::from_value(args["files"].clone())
        .map_err(|_| "Give `files` as a list of paths on this tab's computer.")?;
    if paths.is_empty()
        || paths.len() > agent_upload::MAX_AGENT_FILES
        || paths.iter().any(|p| p.trim().is_empty())
    {
        return Err("Give 1 to 8 files to upload.".into());
    }
    let chooser = tab.upload_chooser().await?;
    if !chooser.many && paths.len() > 1 {
        return Err("This file picker accepts one file, not several.".into());
    }
    let allowed_under = policy(ctx).ok_or(POLICY_CHANGED)?;
    let machine_rule = machine_rule(tab, ctx)?;
    let destination = url::Url::parse(&chooser.url)
        .map_err(|_| "The destination cannot be checked.")?
        .origin()
        .ascii_serialization();
    // An always-ask machine also confirms the preparatory read. Its bytes
    // are not model output, but must not leave that machine unconfirmed.
    if tab.machine().is_some() && machine_rule == tessera_proto::MachineAsk::Always {
        ctx.host.approve_reason(Approve {
            session_id:ctx.session_id.clone(), call_id:ctx.call_id.clone(), tool:"browser".into(),
            summary:format!("Read {} on the tab's machine to prepare upload to {destination}", paths.join(", ")),
            reason:"This machine asks before every action. Read bounded copies for secret checks and a separate upload confirmation; nothing is sent to the website yet.".into(),
            key:format!("browser:upload:prepare:{}",ctx.call_id), always:true, once:true,
            cancel:ctx.cancel.clone(), arguments:json!({"files":paths,"destination":destination}),
        }).await?;
    }
    let mut read_actor = tab.upload_actor(&ctx.session_id)?;
    read_actor["uploadAsk"] = json!(machine_rule);
    read_actor["what"] = json!(format!(
        "Prepare {} for {destination}; nothing sent to the website yet",
        paths.join(", ")
    ));
    let sources: Sources = if tab.machine().is_some() {
        tab.remote_upload_sources(&paths, &read_actor).await?
    } else {
        let paths = paths.iter().map(|p| ctx.resolve(p)).collect::<Vec<_>>();
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || {
            agent_upload::read_sources(&paths, |p| local_source(p, &ctx))
        })
        .await
        .map_err(|e| e.to_string())??
    };
    // The machine never returns these bytes as tool output. Scan against the
    // server's saved secrets too, including secrets not available to this bot.
    sources.validate(&ctx.host.secrets())?;
    let manifest = sources.manifest();
    ctx.host.approve_reason(Approve {
        session_id: ctx.session_id.clone(), call_id: ctx.call_id.clone(), tool: "browser".into(),
        summary: format!("Upload {manifest} to {destination}"),
        reason: format!("Sends these files to the page at {destination}. The page can read and forward them immediately, before any Submit button is pressed."),
        key: format!("browser:upload:{}", ctx.call_id), always: machine_rule != tessera_proto::MachineAsk::Chat,
        cancel: ctx.cancel.clone(), arguments: json!({"action":"upload","files":sources.paths,"destination":destination}), once: true,
    }).await?;
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    if policy(ctx).as_ref() != Some(&allowed_under) {
        return Err(POLICY_CHANGED.into());
    }
    let browsers = ctx.host.browser().ok_or("The browser closed.")?;
    let mark = browsers.turn_mark(tab, &ctx.session_id);
    browsers
        .before_act_as(
            tab,
            &ctx.session_id,
            mark,
            "upload",
            &format!("{manifest} to {destination}"),
        )
        .await?;
    sources.validate(&ctx.host.secrets())?;
    if tab.machine().is_none() {
        for path in &sources.paths {
            local_source(std::path::Path::new(path), ctx)?;
        }
    }
    if self::machine_rule(tab, ctx)? != machine_rule {
        return Err("The machine's confirmation settings changed. Nothing was sent.".into());
    }
    let mut actor = tab.upload_actor(&ctx.session_id)?;
    actor["uploadAsk"] = json!(machine_rule);
    actor["what"] = json!(format!("{manifest} to {destination}"));
    tab.send_agent_files(&chooser, &sources, &actor).await?;
    tab.settle().await;
    Ok(format!(
        "Sent {manifest} to {destination}. The page now has these files."
    ))
}
