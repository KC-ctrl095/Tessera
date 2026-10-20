//! `browser`: the agent's hands in the browser web views run in, the same
//! tabs the user's web view tiles show (so they can watch, and the agent
//! sees what they did), plus headless tabs of its own. A web view runs in
//! this server's browser or in the one of the machine the user is using the
//! chat on ("Web views here"; `on` picks one): tabs on a machine are only
//! for chats that may use it, and the machine checks and logs every action.
//!
//! The agent works in background tabs: nothing it does puts a tile on the
//! user's screen. Each step's card in the chat carries a small picture of
//! the page and lets the user watch. Only `show` (the user asked to see it)
//! and `needs_user` (a sign-in, CAPTCHA or check only the user can do; it
//! waits for them) put a tab on screen.
//!
//! Policy: navigation follows `web_fetch` (local-network addresses ask);
//! typing into password fields, submitting forms, buying/sending-type
//! clicks and downloads ask unless the session is in auto mode; stored
//! secrets are typed only as `$NAME`, only on the secret's hosts, and ask
//! (even in auto mode, unless the secret says not to). Page text passes the
//! injection scan. Cookies, storage and page scripts are never exposed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{
    AskAnswer, AskInput, BoardContent, BrowserNavAction, ContentPart, PermissionMode,
};

use super::{Approve, Risk, Tool, ToolCtx, ToolKind, ToolOutput, opt_bool, opt_str, opt_u64};
use crate::browser::{Browsers, ElementInfo, Tab};

pub struct BrowserTool;

#[path = "browser_upload.rs"]
mod uploads;

/// Characters of outline a `snapshot` returns, and after navigating.
const SNAPSHOT_CHARS: usize = 12_000;
const NAV_SNAPSHOT_CHARS: usize = 6_000;
const MAX_WAIT_MS: u64 = 60_000;
/// The picture of the page on each step's card: its long side, quality.
const THUMB_SIDE: u32 = 240;
const THUMB_QUALITY: u8 = 50;

const ACTIONS: [&str; 19] = [
    "open",
    "tabs",
    "navigate",
    "back",
    "forward",
    "reload",
    "snapshot",
    "click",
    "upload",
    "type",
    "press",
    "select",
    "scroll",
    "wait",
    "screenshot",
    "dialog",
    "show",
    "needs_user",
    "close",
];

impl Tool for BrowserTool {
    fn name(&self) -> &'static str {
        "browser"
    }
    fn description(&self) -> String {
        // Kept short (it is in every request): `docs browser` has the rest.
        "Browser with the user's logins, in background tabs. `show` a tab only if the user asks to see it. \
         `needs_user` + `reason` hands it to them and waits (sign-in, CAPTCHA). \
         `web_fetch` for plain pages. `open` a URL, `snapshot` for refs to act. Read `docs browser`."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ACTIONS},
                "tab": {"type": "string"},
                "url": {"type": "string"},
                "ref": {"type": "string"},
                "text": {"type": "string"},
                "key": {"type": "string"},
                "x": {"type": "number"},
                "y": {"type": "number"},
                "dy": {"type": "number"},
                "ms": {"type": "integer"},
                "reason": {"type": "string"},
                "on": {"type": "string"},
                "submit": {"type": "boolean"},
                "accept": {"type": "boolean"},
                "files": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, _args: &Value) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("snapshot");
        if action == "upload" {
            return format!(
                "upload · {}",
                args["files"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            );
        }
        let what = opt_str(args, "reason")
            .filter(|_| matches!(action, "show" | "needs_user"))
            .or_else(|| opt_str(args, "url"))
            .or_else(|| opt_str(args, "key"))
            .or_else(|| opt_str(args, "ref"))
            .or_else(|| opt_str(args, "text"))
            .map(|s| crate::util::one_line(s, 80));
        match what {
            Some(w) => format!("{action} · {w}"),
            None => action.to_string(),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let action = opt_str(args, "action").unwrap_or("");
        let mode = ctx.host.permission(&ctx.session_id);
        if matches!(action, "open" | "navigate" | "needs_user")
            && let Some(url) = opt_str(args, "url")
            && let Ok(url) = crate::browser::normalize_address(url)
        {
            let risk = super::web::fetch_risk(&url);
            if risk != Risk::None {
                return risk;
            }
        }
        let acts = matches!(action, "click" | "upload" | "type" | "press" | "select")
            || (action == "dialog" && opt_bool(args, "accept") == Some(true));
        match mode {
            PermissionMode::ReadOnly if acts => Risk::Block {
                reason: "This session is read-only: browse and read pages, but don't click, type or submit.".into(),
            },
            PermissionMode::Ask if acts && action != "upload" => Risk::Ask {
                reason: "acts on a web page".into(),
                key: "browser:act".into(),
            },
            _ => Risk::None,
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        // An agent's call: on a machine's browser it closes no page to make
        // room unless that machine allowed it on that run.
        Box::pin(crate::browser::as_agent(async move {
            let Some(browsers) = ctx.host.browser() else {
                return ToolOutput::error("The browser isn't available here.");
            };
            let action = opt_str(&args, "action").unwrap_or("snapshot").to_string();
            if !ACTIONS.contains(&action.as_str()) {
                return ToolOutput::error(format!(
                    "Unknown action `{action}`; one of: {}.",
                    ACTIONS.join(", ")
                ));
            }
            if args.get("files").is_some() && action != "upload" {
                return ToolOutput::error(
                    "Use action `upload` with `files` after clicking the page's file picker.",
                );
            }
            let boards = web_boards(ctx);
            let out = match action.as_str() {
                "tabs" => Ok(tabs(&browsers, ctx, &boards)),
                "open" => open(&browsers, &args, ctx).await,
                "show" => show(&browsers, &args, ctx, &boards).await,
                "needs_user" => needs_user(&browsers, &args, ctx, &boards).await,
                "close" => close(&browsers, &args, ctx, &boards).await,
                _ => {
                    let base =
                        match browsers.agent_tab(&ctx.session_id, opt_str(&args, "tab"), &boards) {
                            Ok(t) => t,
                            Err(e) => return ToolOutput::error(cap(&e)),
                        };
                    touch(&base);
                    // Its turn's count of Stops on the tab's machine, taken
                    // now: this work stays its turn's whatever starts later.
                    let mark = browsers.turn_mark(&base, &ctx.session_id);
                    // What the machine logs: the address, element or key
                    // (never what is typed), once it is known to be valid.
                    let what = match action.as_str() {
                        "navigate" => match crate::browser::normalize_address(
                            opt_str(&args, "url").unwrap_or_default(),
                        ) {
                            Ok(u) => u,
                            Err(e) => return ToolOutput::error(cap(&e)),
                        },
                        "click" => opt_str(&args, "ref")
                            .or_else(|| opt_str(&args, "text"))
                            .unwrap_or_default()
                            .to_string(),
                        _ => opt_str(&args, "ref")
                            .or_else(|| opt_str(&args, "key"))
                            .unwrap_or_default()
                            .to_string(),
                    };
                    // Asked of a machine before its tab is brought back (that
                    // loads its page); never for a call already stopped.
                    if ctx.cancel.is_cancelled() {
                        return ToolOutput::error("Stopped.");
                    }
                    let allowed_under = policy(ctx);
                    let admitted = match browsers
                        .before_act_as(&base, &ctx.session_id, mark, &action, &what)
                        .await
                    {
                        Ok(a) => a,
                        Err(e) => return ToolOutput::error(cap(&e)),
                    };
                    // Only on the run of the machine's browser it was allowed
                    // on, to its end (its page is never used on a run started
                    // since, also after waiting for another action).
                    let out = crate::browser::on_run(admitted, async {
                        base.ensure_live_on(admitted).await?;
                        // A popup open on the tab is where the action goes.
                        let tab = base.top();
                        let _acting = tab.acting().await;
                        // Waited for another action: only under what it was
                        // let through with.
                        if allowed_under.is_none() || policy(ctx) != allowed_under {
                            return Err(POLICY_CHANGED.into());
                        }
                        let out = act(&action, &tab, &base, &args, ctx).await;
                        still_on_run(&[&base, &tab])?;
                        out
                    })
                    .await;
                    touch(&base);
                    // What happened to its page since the agent's last step
                    // (the browser stopped, the page crashed), once.
                    match base.take_interrupted() {
                        Some(note) => with_note(out, &note),
                        None => out,
                    }
                }
            };
            match out {
                Ok(o) => o,
                Err(e) => ToolOutput::error(cap(&e)),
            }
        }))
    }
}

/// What a browser action is let through under: the chat's mode and its
/// bot's settings (its access among them). `None` when its bot's settings
/// can't be read (then nothing is let through on them).
fn policy(ctx: &ToolCtx) -> Option<(PermissionMode, Option<(String, u64)>)> {
    let bot = match ctx.host.session_bot(&ctx.session_id) {
        Some(id) => {
            let updated = ctx
                .host
                .store()
                .bots()
                .ok()?
                .into_iter()
                .find(|b| b.id == id)?
                .updated_at;
            Some((id, updated))
        }
        None => None,
    };
    Some((ctx.host.permission(&ctx.session_id), bot))
}

const POLICY_CHANGED: &str = "Not done: this chat's permissions (or its bot's access) changed, or couldn't be checked, while it waited for another action on the page; ask again if it should still happen.";

/// The page went to another run of its browser while an action's answer
/// was made (a Stop, a restart): nothing of that one in the answer. (The
/// action itself may close a tab, a popup's `back` say: that's no error.)
fn still_on_run(tabs: &[&Tab]) -> Result<(), String> {
    for t in tabs {
        if let Err(e) = t.link()
            && e == crate::browser::RESTARTED
        {
            return Err(e);
        }
    }
    Ok(())
}

/// A page just read is still open on the run it was read on (`open`,
/// `needs_user`): closed or moved meanwhile (a Stop), nothing of it in the
/// answer.
fn still_open(tabs: &[&Tab]) -> Result<(), String> {
    for t in tabs {
        t.link()?;
    }
    Ok(())
}

/// `note` in front of an action's answer (or its error).
fn with_note(out: Result<ToolOutput, String>, note: &str) -> Result<ToolOutput, String> {
    match out {
        Ok(mut o) => {
            match o.content.first_mut() {
                Some(ContentPart::Text { text }) => *text = format!("{note}\n{text}"),
                _ => o.content.insert(0, ContentPart::text(note)),
            }
            Ok(o)
        }
        Err(e) => Err(format!("{note} Then: {e}")),
    }
}

/// The tab's browser is in use (it doesn't stop for being idle).
fn touch(tab: &Tab) {
    if let Some(b) = tab.host_browser() {
        b.touch();
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => {
            let mut out: String = f.to_uppercase().collect();
            out.push_str(c.as_str());
            if !out.ends_with(['.', '!', '?', ')']) {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

/// The web view boards (`Url`/`Local`) this agent can see.
fn web_boards(ctx: &ToolCtx) -> Vec<String> {
    ctx.host
        .board_list(&ctx.session_id)
        .into_iter()
        .filter(|v| {
            matches!(
                v.board.content,
                BoardContent::Url { .. } | BoardContent::Local { .. }
            )
        })
        .map(|v| v.board.id)
        .collect()
}

/// For the transcript's tool card: which tab (and tile) this was about.
fn details(tab: &Tab, action: &str) -> Value {
    let mut d = json!({
        "tab": tab.key(),
        "board_id": tab.board(),
        "url": tab.url(),
        "title": tab.title(),
        "action": action,
        "popup": tab.is_popup(),
    });
    if let Some(m) = tab.machine() {
        d["machine"] = json!(m);
    }
    if let Some(b) = tab.board() {
        // The same shape as the `whiteboard` tool's (opens the tile).
        d["board"] = json!({"id": b, "title": tab.title(), "kind": "url"});
    }
    d
}

fn tabs(browsers: &Browsers, ctx: &ToolCtx, boards: &[String]) -> ToolOutput {
    let list = browsers.agent_tabs(&ctx.session_id, boards);
    if list.is_empty() {
        return ToolOutput::text("No tabs. `open` a url.");
    }
    let current = browsers
        .agent_tab(&ctx.session_id, None, boards)
        .ok()
        .map(|t| t.key());
    let lines: Vec<String> = list
        .iter()
        .map(|t| {
            let mark = if current.as_deref() == Some(t.key.as_str()) {
                "*"
            } else {
                " "
            };
            let mut s = format!("{mark} {} · ", t.key);
            if !t.title.is_empty() {
                s.push_str(&format!("\"{}\" · ", crate::util::one_line(&t.title, 60)));
            }
            s.push_str(&crate::util::one_line(&t.url, 120));
            s.push_str(if t.board.is_some() {
                " · web view tile"
            } else {
                " · background (yours)"
            });
            s.push_str(&browsers.on_where(t.machine.as_deref()));
            if let Some(p) = &t.popup {
                s.push_str(&format!(
                    " · popup open on top: {}",
                    crate::util::one_line(p, 100)
                ));
            }
            if t.watched {
                s.push_str(" · the user is watching");
            }
            s
        })
        .collect();
    ToolOutput::text(format!("Tabs (* = current):\n{}", lines.join("\n")))
}

/// A new background tab at `url` (where: `on`, else the machine the user is
/// using this chat on when it allows web views, else this server). It never
/// goes on screen by itself. With its turn's count of Stops there (taken
/// before anything was waited for) and the run of the machine's browser it
/// was allowed on.
async fn open_tab(
    browsers: &Browsers,
    args: &Value,
    ctx: &ToolCtx,
) -> Result<(Arc<Tab>, String, Option<u32>, Option<u64>), String> {
    let url = crate::browser::normalize_address(opt_str(args, "url").ok_or("`url` is required")?)?;
    let host = browsers.place(&ctx.session_id, opt_str(args, "on"))?;
    let at = browsers.on_where(host.as_deref());
    // Asked of its machine before allocating a handle, with this call's
    // count of Stops. At the per-chat cap, allocation refuses without closing.
    let mark = browsers.turn_mark_on(host.as_deref(), &ctx.session_id);
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let admitted = browsers
        .admit_on(host.as_deref(), &ctx.session_id, mark, "open", &url)
        .await?;
    let tab = crate::browser::on_run(admitted, async {
        browsers.new_agent_tab(&ctx.session_id, "", host.as_deref())
    })
    .await?;
    touch(&tab);
    let gone = crate::browser::on_run(admitted, async {
        tab.ensure_live_on(admitted).await?;
        let _acting = tab.acting().await;
        let gone = tab.go(&url).await;
        tab.wait_load(Duration::from_secs(15)).await;
        gone
    })
    .await;
    if let Err(e) = gone {
        browsers.close_agent_tab(&ctx.session_id, &tab.key());
        return Err(format!("{e} (tab `{}`)", tab.key()));
    }
    Ok((tab, at, mark, admitted))
}

async fn open(browsers: &Browsers, args: &Value, ctx: &ToolCtx) -> Result<ToolOutput, String> {
    let (tab, at, _, admitted) = open_tab(browsers, args, ctx).await?;
    let mut head = format!("Opened background tab `{}`{at}.", tab.key());
    // Older instructions ask for a tile with `open`: the page stays in the
    // background, and the agent learns how to put it on screen.
    if opt_bool(args, "show") == Some(true) {
        head.push_str(
            " It is not on screen: `show` it if the user asked to see it, or `needs_user` with a \
             `reason` if they must sign in or act there.",
        );
    }
    crate::browser::on_run(admitted, async {
        let out = page_result(&tab, None, &head, NAV_SNAPSHOT_CHARS, "open").await?;
        still_open(&[&tab])?;
        Ok(out)
    })
    .await
}

/// Put `tab` on screen: its tile beside the chat (a background tab becomes
/// one as it is, same page and form contents). Its board's id.
async fn on_screen(browsers: &Browsers, tab: &Arc<Tab>, ctx: &ToolCtx) -> Result<String, String> {
    if let Some(b) = tab.board() {
        // A web view the user closed comes back.
        ctx.host.board_tile(&ctx.session_id, &b)?;
        return Ok(b);
    }
    let url = tab.url();
    if !url.starts_with("http") {
        return Err("open a page in it first".into());
    }
    let title = Some(tab.title()).filter(|t| !t.is_empty());
    // The tile shows this very page, so it runs where the tab does.
    let board = ctx
        .host
        .web_board(&ctx.session_id, title.as_deref(), &url, tab.machine())?;
    let chat = board.session_id.clone();
    browsers.adopt(tab, &board.id, &chat, &ctx.session_id);
    tab.sync_view().await;
    Ok(board.id)
}

async fn show(
    browsers: &Browsers,
    args: &Value,
    ctx: &ToolCtx,
    boards: &[String],
) -> Result<ToolOutput, String> {
    let tab = browsers.agent_tab(&ctx.session_id, opt_str(args, "tab"), boards)?;
    // As any action there: asked of its machine with this call's count of
    // Stops, and its picture taken only on the run that allowed it.
    let mark = browsers.turn_mark(&tab, &ctx.session_id);
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let admitted = browsers
        .before_act_as(&tab, &ctx.session_id, mark, "show", "")
        .await?;
    crate::browser::on_run(admitted, async {
        let board = on_screen(browsers, &tab, ctx).await?;
        let out = ToolOutput::text(format!(
            "Tab `{board}` is in a tile beside the chat: the user sees this page as it is."
        ))
        .with_details(details(&tab, "show"));
        let out = with_shot(&tab, out).await;
        if let Err(e) = tab.link()
            && e == crate::browser::RESTARTED
        {
            return Err(e);
        }
        Ok(out)
    })
    .await
}

/// Hand a tab to the user for what only they can do (sign in without a
/// stored login, a CAPTCHA, a check): its tile goes on screen with `reason`
/// and Done / I can't, and the agent waits for them.
async fn needs_user(
    browsers: &Browsers,
    args: &Value,
    ctx: &ToolCtx,
    boards: &[String],
) -> Result<ToolOutput, String> {
    let reason = opt_str(args, "reason")
        .map(|r| crate::util::one_line(r, 300))
        .filter(|r| !r.trim().is_empty())
        .ok_or(
            "`reason` is required: what the user should do there, e.g. \"Sign in to the router\"",
        )?;
    let (tab, mark) = match (opt_str(args, "tab"), opt_str(args, "url")) {
        (None, Some(_)) => {
            let (t, _, mark, _) = open_tab(browsers, args, ctx).await?;
            (t, mark)
        }
        (tab, _) => {
            let t = browsers.agent_tab(&ctx.session_id, tab, boards)?;
            let mark = browsers.turn_mark(&t, &ctx.session_id);
            (t, mark)
        }
    };
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let admitted = browsers
        .before_act_as(&tab, &ctx.session_id, mark, "show", "")
        .await?;
    let board = crate::browser::on_run(admitted, on_screen(browsers, &tab, ctx)).await?;
    (ctx.progress)(&format!("Waiting for the user: {reason}\n"));
    let answer = ctx
        .host
        .board_ask(
            &ctx.session_id,
            Some(board.clone()),
            None,
            reason,
            AskInput::Confirm,
            true,
            ctx.cancel.clone(),
        )
        .await;
    let head = match answer {
        Ok(AskAnswer::Confirm { yes: true }) => "The user is done.",
        Ok(AskAnswer::Confirm { yes: false }) => {
            "The user couldn't do it: don't ask again; continue another way or ask them in the chat."
        }
        Ok(_) => return Ok(ToolOutput::text(DECLINED)),
        Err(e) => return Err(e),
    };
    // What the page is now (the tab is the tile's), as the agent's work: the
    // machine is asked again before its page is brought back.
    let tab = browsers.agent_tab(&ctx.session_id, Some(&board), &web_boards(ctx))?;
    touch(&tab);
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let admitted = browsers
        .before_act_as(&tab, &ctx.session_id, mark, "snapshot", "")
        .await?;
    crate::browser::on_run(admitted, async {
        tab.ensure_live_on(admitted).await?;
        let top = tab.top();
        let out = page_result(&top, None, head, NAV_SNAPSHOT_CHARS, "needs_user").await?;
        still_open(&[&tab, &top])?;
        Ok(out)
    })
    .await
}

/// Pages a stored secret was typed into, by tab (its address then): no
/// picture of them until the tab is on another page.
fn secret_pages() -> &'static parking_lot::Mutex<std::collections::HashMap<usize, String>> {
    static PAGES: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<usize, String>>,
    > = std::sync::OnceLock::new();
    PAGES.get_or_init(Default::default)
}

/// A tab's identity for [`secret_pages`] (it keeps it when it becomes a
/// board's).
fn tab_id(tab: &Tab) -> usize {
    tab as *const Tab as usize
}

/// The step's card gets a small picture of the page (`details.shot`),
/// except of a page a stored secret was typed into.
async fn with_shot(tab: &Tab, mut out: ToolOutput) -> ToolOutput {
    {
        let mut pages = secret_pages().lock();
        match pages.get(&tab_id(tab)) {
            Some(url) if *url == tab.url() => return out,
            Some(_) => {
                pages.remove(&tab_id(tab));
            }
            None => {}
        }
    }
    let shot = tokio::time::timeout(
        Duration::from_millis(1500),
        tab.screenshot(THUMB_SIDE, THUMB_QUALITY),
    )
    .await;
    if let (Ok(Ok(shot)), Some(d)) = (shot, out.details.as_mut()) {
        d["shot"] = json!({
            "mime": shot.image.mime,
            "data": shot.image.data,
            "width": shot.image.width,
            "height": shot.image.height,
        });
    }
    out
}

async fn close(
    browsers: &Browsers,
    args: &Value,
    ctx: &ToolCtx,
    boards: &[String],
) -> Result<ToolOutput, String> {
    let tab = browsers.agent_tab(&ctx.session_id, opt_str(args, "tab"), boards)?;
    let key = tab.key();
    // Closing a page open on a machine is an action there: asked of it with
    // this call's count of Stops, on the run it allows.
    let mark = browsers.turn_mark(&tab, &ctx.session_id);
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    let admitted = if tab.is_live() {
        browsers
            .before_act_as(&tab, &ctx.session_id, mark, "close", "")
            .await?
    } else {
        None
    };
    crate::browser::on_run(admitted, async {
        match tab.board() {
            None => {
                browsers.close_agent_tab(&ctx.session_id, &key);
                Ok(ToolOutput::text(format!("Closed tab `{key}`.")))
            }
            Some(b) => {
                let by_user = ctx
                    .host
                    .board_list(&ctx.session_id)
                    .iter()
                    .any(|v| v.board.id == b && v.board.by_user);
                if by_user {
                    return Err("that tile is the user's: leave it open".into());
                }
                ctx.host.board_delete(&ctx.session_id, &b)?;
                browsers.forget_recent(&ctx.session_id);
                Ok(ToolOutput::text(format!("Closed the tile `{b}`.")))
            }
        }
    })
    .await
}

/// The page's host (for approvals and secrets).
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

async fn ask(
    ctx: &ToolCtx,
    tab: &Tab,
    summary: String,
    reason: String,
    key: String,
    always: bool,
) -> Result<(), String> {
    let allowed = ctx
        .host
        .approve_reason(Approve {
            session_id: ctx.session_id.clone(),
            call_id: ctx.call_id.clone(),
            tool: "browser".into(),
            summary,
            reason,
            key,
            always,
            cancel: ctx.cancel.clone(),
            arguments: serde_json::json!({}),
            once: false,
        })
        .await;
    // A page on a machine: that machine must still allow this chat there
    // (paused, web views off, a bot's access narrowed while it waited).
    allowed?;
    if let Some(b) = ctx.host.browser() {
        b.still_usable(tab, &ctx.session_id)?;
    }
    Ok(())
}

const DECLINED: &str =
    "The user declined this. Don't retry it; continue another way or ask the user.";

/// Where an action points: `ref`, `text` (click only) or `x`,`y` in the
/// pixels of the latest screenshot.
fn target(args: &Value, tab: &Tab, by_text: bool) -> Option<Value> {
    if let Some(r) = opt_str(args, "ref") {
        return Some(json!({"ref": r.trim().trim_matches(['[', ']'])}));
    }
    if let (Some(x), Some(y)) = (
        args.get("x").and_then(Value::as_f64),
        args.get("y").and_then(Value::as_f64),
    ) {
        let s = tab.with_state(|st| st.shot_scale).max(0.01);
        return Some(json!({"x": x / s, "y": y / s}));
    }
    if by_text && let Some(t) = opt_str(args, "text") {
        return Some(json!({"text": t}));
    }
    None
}

/// Whether submitting `e`'s form needs the user (not a search box).
fn submits(e: &ElementInfo) -> bool {
    e.form
        .as_ref()
        .is_some_and(|f| !f.search && (f.method == "post" || f.password))
}

async fn act(
    action: &str,
    tab: &Arc<Tab>,
    base: &Arc<Tab>,
    args: &Value,
    ctx: &ToolCtx,
) -> Result<ToolOutput, String> {
    let browser = tab.host_browser().ok_or("the browser is shutting down")?;
    let started = Instant::now();
    let url_before = tab.url();
    let host = host_of(&url_before);
    let mut note;
    // Downloads the user already allowed with the click that starts them.
    let mut download_ok = false;
    match action {
        "upload" => note = uploads::upload(tab, args, ctx).await?,
        "navigate" => {
            let url = crate::browser::normalize_address(
                opt_str(args, "url").ok_or("`url` is required")?,
            )?;
            tab.go(&url).await?;
            tab.wait_load(Duration::from_secs(15)).await;
            return page_result(tab, None, "", NAV_SNAPSHOT_CHARS, action).await;
        }
        "back" | "forward" | "reload" => {
            tab.nav(match action {
                "back" => BrowserNavAction::Back,
                "forward" => BrowserNavAction::Forward,
                _ => BrowserNavAction::Reload,
            })
            .await?;
            let now = base.top();
            if !Arc::ptr_eq(&now, tab) {
                now.settle().await;
                return page_result(
                    &now,
                    None,
                    "Closed the popup; back on the page that opened it.",
                    NAV_SNAPSHOT_CHARS,
                    action,
                )
                .await;
            }
            tab.settle().await;
            tab.wait_load(Duration::from_secs(15)).await;
            return page_result(tab, None, "", NAV_SNAPSHOT_CHARS, action).await;
        }
        "snapshot" => {
            return page_result(tab, opt_str(args, "text"), "", SNAPSHOT_CHARS, action).await;
        }
        "screenshot" => {
            if !ctx.host.sees_images(&ctx.session_id) {
                return Err("this model can't see images: use `snapshot` to read the page".into());
            }
            let shot = tab.screenshot(1280, 70).await?;
            tab.with_state(|st| st.shot_scale = shot.scale);
            return Ok(ToolOutput {
                content: vec![
                    ContentPart::text(format!(
                        "{} — {} ({}×{}; `click` with x,y in these pixels)",
                        tab.title(),
                        tab.url(),
                        shot.image.width,
                        shot.image.height
                    )),
                    ContentPart::Image {
                        mime: shot.image.mime,
                        data: shot.image.data,
                    },
                ],
                is_error: false,
                details: Some(details(tab, action)),
            });
        }
        "wait" => {
            let ms = opt_u64(args, "ms").unwrap_or(10_000).min(MAX_WAIT_MS);
            let msg = match opt_str(args, "text") {
                Some(t) => {
                    let found = tokio::select! {
                        r = tab.wait_text(t, ms, false) => r?,
                        _ = ctx.cancel.cancelled() => return Err("stopped".into()),
                    };
                    if !found {
                        return Err(format!(
                            "\"{t}\" didn't appear within {:.1} s",
                            ms as f64 / 1000.0
                        ));
                    }
                    format!("\"{t}\" is on the page.")
                }
                None => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
                        _ = ctx.cancel.cancelled() => return Err("stopped".into()),
                    }
                    tab.wait_load(Duration::from_secs(10)).await;
                    format!("Waited {:.1} s.", ms as f64 / 1000.0)
                }
            };
            tab.refresh().await;
            let out =
                ToolOutput::text(format!("{msg} {}", line(tab))).with_details(details(tab, action));
            return Ok(with_shot(tab, out).await);
        }
        "dialog" => {
            let Some((kind, message, _)) = tab.dialog() else {
                return Err("no dialog is open".into());
            };
            let accept = opt_bool(args, "accept").unwrap_or(true);
            tab.answer_dialog(accept, opt_str(args, "text").map(str::to_string))
                .await?;
            tab.settle().await;
            note = format!(
                "{} the {} \"{}\".",
                if accept { "Accepted" } else { "Dismissed" },
                format!("{kind:?}").to_lowercase(),
                crate::util::one_line(&message, 120)
            );
        }
        "scroll" => {
            let (vw, vh) = {
                let (v, z) = tab.view();
                (v.width as f64 / z, v.height as f64 / z)
            };
            if let Some(t) = opt_str(args, "ref").map(|r| json!({"ref": r})) {
                let e = tab.reveal(&t).await?;
                note = format!("Scrolled {} into view.", e.describe());
            } else {
                let s = tab.with_state(|st| st.shot_scale).max(0.01);
                let x = args
                    .get("x")
                    .and_then(Value::as_f64)
                    .map_or(vw / 2.0, |x| x / s);
                let y = args
                    .get("y")
                    .and_then(Value::as_f64)
                    .map_or(vh / 2.0, |y| y / s);
                let dy = args
                    .get("dy")
                    .and_then(Value::as_f64)
                    .unwrap_or(vh * 0.8)
                    .clamp(-50_000.0, 50_000.0);
                tab.wheel(x, y, 0.0, dy).await?;
                note = format!(
                    "Scrolled {} {} px.",
                    if dy < 0.0 { "up" } else { "down" },
                    dy.abs().round()
                );
            }
        }
        "select" => {
            let t = target(args, tab, false).ok_or("`ref` of the list is required")?;
            let value = opt_str(args, "text").ok_or("`text` (the option) is required")?;
            let e = tab.probe(&t, true).await?;
            let chosen = tab.select(&t, &[value.to_string()]).await?;
            tab.settle().await;
            note = format!("Selected \"{}\" in {}.", chosen.join(", "), e.describe());
        }
        "click" => {
            let t = target(args, tab, true)
                .ok_or("give the element's `ref` (from a snapshot), its `text`, or `x`,`y`")?;
            let e = tab.probe(&t, true).await?;
            if e.disabled {
                return Err(format!("{} is disabled", e.describe()));
            }
            if !e.covered.is_empty() && t.get("x").is_none() {
                return Err(format!(
                    "{} is covered by {} (a banner or dialog?): deal with that first",
                    e.describe(),
                    e.covered
                ));
            }
            let what = e.describe();
            let mut why = Vec::new();
            if e.sensitive {
                why.push((
                    "looks like it buys, sends, posts or deletes something".to_string(),
                    format!("browser:act:{host}"),
                ));
            }
            if e.submit && submits(&e) {
                why.push((
                    "submits a form".to_string(),
                    format!("browser:submit:{host}"),
                ));
            }
            if e.download {
                why.push((
                    "downloads a file".to_string(),
                    format!("browser:download:{host}"),
                ));
            }
            if !e.href.is_empty()
                && let Ok(u) = url::Url::parse(&e.href)
                && super::web::internal_url(&u)
                && url::Url::parse(&url_before).is_ok_and(|p| !super::web::internal_url(&p))
            {
                why.push((
                    format!(
                        "opens a local network address ({})",
                        u.host_str().unwrap_or("")
                    ),
                    format!("web:{}", u.host_str().unwrap_or("")),
                ));
            }
            download_ok = e.download;
            for (reason, key) in why {
                let summary = format!("click {what} on {host}");
                ask(
                    ctx,
                    tab,
                    summary,
                    format!("This click {reason}."),
                    key,
                    false,
                )
                .await?;
            }
            tab.flash(&e).await;
            // What the click changes, to say so (or that nothing did).
            let watching = tab.watch_changes(&t).await;
            tab.click_at(e.x, e.y, 1).await?;
            tab.settle().await;
            let at = if t.get("x").is_some() {
                format!(" at ({:.0}, {:.0})", e.x, e.y)
            } else {
                String::new()
            };
            note = format!("Clicked {what}{at}.");
            // A new page, a popup, a dialog or a download says itself below.
            let said = tab.with_state(|st| {
                st.downloads.iter().any(|(_, t)| *t >= started)
                    || st.chooser.is_some_and(|t| t >= started)
                    || st.window_opened.is_some_and(|t| t >= started)
            });
            if watching
                && !said
                && tab.url() == url_before
                && tab.dialog().is_none()
                && Arc::ptr_eq(&base.top(), tab)
                && let Some(c) = tab.page_changes().await
            {
                note.push(' ');
                note.push_str(&c.describe());
            }
        }
        "type" => {
            let raw = opt_str(args, "text")
                .ok_or("`text` is required")?
                .to_string();
            let target = target(args, tab, false);
            let e = match &target {
                Some(t) => tab.focus_field(t).await?,
                None => match tab.focused().await {
                    Some(e) if e.editable => e,
                    _ => return Err("give the field's `ref` (no text field has focus)".into()),
                },
            };
            let what = e.describe();
            // Stored secrets: only as $NAME, only on their hosts, asked first.
            let uses = ctx
                .host
                .secret_env(&ctx.session_id, &raw, crate::auth::SecretPlace::Server);
            if let Some(why) = super::secrets::elsewhere(
                &uses,
                || ctx.host.machines(),
                crate::auth::SecretPlace::Server,
            ) {
                return Err(why);
            }
            let mut text = raw.clone();
            let secret_names: Vec<String> = uses.used.iter().map(|(n, ..)| n.clone()).collect();
            if !uses.used.is_empty() {
                for (name, _, hosts) in &uses.used {
                    if !hosts.is_empty() && !hosts.iter().any(|h| host_matches(&host, h)) {
                        return Err(format!(
                            "the secret ${name} is only for {}, not {host}",
                            hosts.join(", ")
                        ));
                    }
                }
                let always = uses.used.iter().any(|(_, ask, _)| *ask);
                let names = secret_names
                    .iter()
                    .map(|n| format!("${n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if always || e.password {
                    let ok = ask(
                        ctx,
                        tab,
                        format!("type {names} into {what} on {host}"),
                        format!(
                            "Types your stored secret{} {names} into {host}.",
                            if secret_names.len() == 1 { "" } else { "s" }
                        ),
                        format!("browser:secret:{}@{host}", secret_names.join("+")),
                        always,
                    )
                    .await;
                    ok?;
                }
                text = substitute(&raw, &uses.env);
                ctx.host.mark_secrets_used(&secret_names);
                secret_pages().lock().insert(tab_id(tab), tab.url());
            } else if e.password {
                ask(
                    ctx,
                    tab,
                    format!("type into {what} on {host}"),
                    "Types into a password field.".into(),
                    format!("browser:password:{host}"),
                    false,
                )
                .await?;
            }
            let submit = opt_bool(args, "submit") == Some(true);
            if submit && submits(&e) {
                ask(
                    ctx,
                    tab,
                    format!("submit the form of {what} on {host}"),
                    "Submits a form.".into(),
                    format!("browser:submit:{host}"),
                    false,
                )
                .await?;
            }
            tab.insert_text(&text).await?;
            if submit {
                tab.press("Enter").await?;
            }
            tab.settle().await;
            let shown = if secret_names.is_empty() && !e.password {
                format!("\"{}\"", crate::util::one_line(&raw, 60))
            } else if secret_names.is_empty() {
                format!("{} characters", raw.chars().count())
            } else {
                raw.clone()
            };
            note = format!(
                "Typed {shown} into {what}{}.",
                if submit { " and pressed Enter" } else { "" }
            );
        }
        "press" => {
            let key = opt_str(args, "key")
                .ok_or("`key` is required, e.g. Enter, Tab, Escape, ArrowDown, Control+a")?;
            if matches!(key.trim().to_ascii_lowercase().as_str(), "enter" | "return")
                && let Some(e) = tab.focused().await
                && submits(&e)
            {
                ask(
                    ctx,
                    tab,
                    format!("press Enter in {} on {host}", e.describe()),
                    "Submits a form.".into(),
                    format!("browser:submit:{host}"),
                    false,
                )
                .await?;
            }
            tab.press(key).await?;
            tab.settle().await;
            note = format!("Pressed {key}.");
        }
        _ => return Err(format!("unknown action `{action}`")),
    }

    // A popup the action opened (sign-in windows…), or one that closed
    // itself when done: continue on what is on top now.
    if tab.with_state(|st| st.window_opened.is_some_and(|t| t >= started)) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Arc::ptr_eq(&base.top(), tab) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    if tab.is_popup() && tab.url() != url_before {
        let deadline = Instant::now() + Duration::from_millis(1500);
        while tab.is_live() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let now = base.top();
    if !Arc::ptr_eq(&now, tab) {
        if now.is_popup() && !tab.is_popup() || now.opener().is_some_and(|o| Arc::ptr_eq(&o, tab)) {
            now.wait_load(Duration::from_secs(15)).await;
            note.push_str(" A popup window opened (it stays connected to the page, e.g. for a sign-in); you are in it now, and `back` closes it.");
        } else {
            now.settle().await;
            note.push_str(" The popup closed; back on the page that opened it.");
        }
        return page_result(&now, None, &note, NAV_SNAPSHOT_CHARS, action).await;
    }
    tab.refresh().await;
    // What the action led to: downloads, file pickers, dialogs, a new page.
    let downloads: Vec<String> = tab.with_state(|st| {
        st.downloads
            .iter()
            .filter(|(_, t)| *t >= started)
            .map(|(g, _)| g.clone())
            .collect()
    });
    for guid in downloads {
        let name = browser.download(&guid).map(|d| d.name).unwrap_or_default();
        let answer = if download_ok {
            Ok(())
        } else {
            ask(
                ctx,
                tab,
                format!("download {name} from {host}"),
                "The page started a download.".into(),
                format!("browser:download:{host}"),
                false,
            )
            .await
        };
        match answer {
            Ok(()) => {
                // Give a small file the moment it needs to finish.
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    match browser.download(&guid) {
                        Some(d) if d.done.is_some() || d.failed => break,
                        _ => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                }
                match browser.download(&guid) {
                Some(d) if d.done.is_some() => note.push_str(&format!(
                    " Downloaded {} to {}{}.",
                    d.name,
                    d.done
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    browser.on_where()
                )),
                Some(d) if d.failed => {
                    note.push_str(&format!(" The download of {} failed.", d.name))
                }
                _ if browser.machine().is_some() => note.push_str(&format!(
                    " Downloading {name} into Tessera's downloads folder{} (still in progress).",
                    browser.on_where()
                )),
                _ => note.push_str(&format!(
                    " Downloading {name} into {} (still in progress).",
                    browser.downloads_dir().display()
                )),
            }
            }
            Err(why) => {
                browser.discard_download(&guid).await;
                note.push_str(&format!(" The download of {name} was cancelled. {why}"));
            }
        }
    }
    if tab.with_state(|st| st.chooser.take().is_some_and(|t| t >= started)) {
        note.push_str(" The page opened a file picker. Use `upload` with `files` (paths on this tab's computer), or the user can choose files in the web view.");
        if let Ok(chooser) = tab.upload_chooser().await {
            if !chooser.accept.is_empty() {
                note.push_str(&format!(
                    " File-type hint: {}.",
                    crate::util::one_line(&chooser.accept, 200)
                ));
            }
            if !chooser.many {
                note.push_str(" Choose one file.");
            }
        }
    }
    if let Some((kind, message, default)) = tab.dialog() {
        note.push_str(&format!(
            " A {} dialog is open: \"{}\"{} — answer with `dialog` (`accept`, and `text` for a prompt).",
            format!("{kind:?}").to_lowercase(),
            crate::util::one_line(&message, 300),
            default.map(|d| format!(" (default \"{d}\")")).unwrap_or_default()
        ));
        return Ok(ToolOutput::text(note).with_details(details(tab, action)));
    }
    if tab.url() != url_before || action == "upload" {
        return page_result(tab, None, &note, NAV_SNAPSHOT_CHARS, action).await;
    }
    let out = ToolOutput::text(format!("{note} {}", line(tab))).with_details(details(tab, action));
    Ok(with_shot(tab, out).await)
}

/// "Page: title — url" plus any navigation error.
fn line(tab: &Tab) -> String {
    let err = tab.with_state(|st| st.nav_error.clone());
    let mut s = format!("Page: {} — {}", tab.title(), tab.url());
    if let Some(e) = err {
        s.push_str(&format!(" ({e})"));
    }
    s
}

/// The page as text: a header, the outline (scanned for injected
/// instructions) and how to see more.
async fn page_result(
    tab: &Tab,
    filter: Option<&str>,
    head: &str,
    max: usize,
    action: &str,
) -> Result<ToolOutput, String> {
    let snap = tab.snapshot(filter, max).await;
    let mut out = String::new();
    if !head.is_empty() {
        out.push_str(head);
        out.push('\n');
    }
    let out = match snap {
        Ok(s) => render(tab, s, out, action),
        // Not on the run it was allowed on: nothing of the page there.
        Err(e)
            if e == crate::browser::RESTARTED
                || tab.link().err().as_deref() == Some(crate::browser::RESTARTED) =>
        {
            out.push_str(&format!("(couldn't read the page: {e})"));
            ToolOutput::text(out)
        }
        Err(e) => {
            out.push_str(&format!("{}\n(couldn't read the page: {e})", line(tab)));
            ToolOutput::text(out).with_details(details(tab, action))
        }
    };
    Ok(with_shot(tab, out).await)
}

fn render(tab: &Tab, snap: crate::browser::Snapshot, mut out: String, action: &str) -> ToolOutput {
    let scan = crate::scan::scan_page(&snap.text);
    let err = tab.with_state(|st| st.nav_error.clone());
    if tab.is_popup() {
        let under = tab.opener().map(|o| o.url()).unwrap_or_default();
        out.push_str(&format!(
            "[a popup window over {}; `back` closes it]\n",
            crate::util::one_line(&under, 120)
        ));
    }
    out.push_str(&format!(
        "{} — {}{}\n[viewport {}×{}, scroll {}/{}]\n",
        crate::util::one_line(&snap.title, 150),
        snap.url,
        err.map(|e| format!(" ({e})")).unwrap_or_default(),
        snap.viewport.0,
        snap.viewport.1,
        snap.scroll.0,
        snap.scroll.1
    ));
    // Hidden characters are common on real pages (and already removed):
    // only text shaped like instructions to the agent is worth a word.
    let flagged = scan.has_instructions();
    if flagged {
        out.push_str(&crate::scan::label("page", &scan));
        out.push('\n');
    }
    if scan.text.trim().is_empty() {
        out.push_str("(nothing readable on the page yet: `wait`, or take a `screenshot`)");
    } else {
        out.push_str(&scan.text);
    }
    if snap.dropped > 0 {
        out.push_str(&format!(
            "\n[… {} more lines: `snapshot` with `text` to find something, or `scroll` and look again]",
            snap.dropped
        ));
    }
    let mut d = details(tab, action);
    // The page as read, not as the tab may say by now.
    d["url"] = json!(snap.url);
    d["title"] = json!(snap.title);
    if d.get("board").is_some() {
        d["board"]["title"] = json!(snap.title);
    }
    if flagged {
        // Once per site and finding in a chat (the engine keeps them).
        let site = host_of(&snap.url);
        let site = if site.is_empty() {
            snap.url.clone()
        } else {
            site
        };
        d["injection"] = json!(format!("a page on {site}: {}", scan.instructions()));
    }
    ToolOutput::text(out).with_details(d)
}

/// `host` is `pattern` or one of its subdomains (`*.x.com`, `https://x.com/`
/// and `x.com:443` all mean x.com).
fn host_matches(host: &str, pattern: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    let p = p.split("://").last().unwrap_or(&p);
    let p = p.split('/').next().unwrap_or(p);
    let p = p.rsplit_once(':').map_or(p, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            p
        }
    });
    let p = p.trim_start_matches("*.").trim_matches('.');
    let h = host.to_ascii_lowercase();
    !p.is_empty() && (h == p || h.ends_with(&format!(".{p}")))
}

/// Replace `$VAR` / `${VAR}` with the values of the secrets in `env`.
fn substitute(text: &str, env: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        if b[i] == b'$' {
            let braced = b.get(i + 1) == Some(&b'{');
            let start = if braced { i + 2 } else { i + 1 };
            let mut end = start;
            while end < b.len() && ident(b[end]) {
                end += 1;
            }
            let name = &text[start..end];
            let closed = !braced || b.get(end) == Some(&b'}');
            if !name.is_empty()
                && closed
                && let Some((_, v)) = env.iter().find(|(k, _)| k == name)
            {
                out.push_str(v);
                i = if braced { end + 1 } else { end };
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap_or('\u{0}');
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_fill_in_only_their_own_names() {
        let env = vec![
            ("GH_PASSWORD".to_string(), "p@ss".to_string()),
            ("GH".to_string(), "tok".to_string()),
        ];
        assert_eq!(substitute("$GH_PASSWORD", &env), "p@ss");
        assert_eq!(substitute("${GH}x and $GH", &env), "tokx and tok");
        assert_eq!(substitute("costs $5 or $OTHER", &env), "costs $5 or $OTHER");
        assert_eq!(substitute("é$GH€", &env), "étok€");
        assert_eq!(substitute("${GH", &env), "${GH");
    }

    #[test]
    fn secret_hosts_match_the_page() {
        assert!(host_matches("github.com", "github.com"));
        assert!(host_matches("gist.github.com", "*.github.com"));
        assert!(host_matches("github.com", "https://github.com/login"));
        assert!(host_matches("github.com", "github.com:443"));
        assert!(!host_matches("evilgithub.com", "github.com"));
        assert!(!host_matches("github.com.evil.io", "github.com"));
        assert!(!host_matches("github.com", ""));
    }
}
