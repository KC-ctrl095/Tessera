//! "Web views here": this machine's browser for web views, driven by its
//! server through the machine's own connection (`node_browser` messages),
//! so pages the user and agents open from here draw on this computer, not
//! on the server (see `crate::browser::hosts`).
//!
//! Security model (user guide: docs/MACHINES.md):
//!
//! * It runs only while "Web views here" is on (`MachineScope::webviews`),
//!   which only this computer can turn on, and stops at once when that is
//!   switched off, or the machine is paused, revoked, turned off or loses
//!   its connection.
//! * The server speaks the DevTools protocol to it, and only an allowlist of
//!   commands passes, with their parameters checked ([`Filter`]): nothing
//!   that reaches outside the page sandbox. No local files (only http(s),
//!   `about:blank` and HTML `data:` addresses), no other targets, profiles
//!   or browser settings. Uploads use selected bytes or checked agent sources,
//!   never raw filesystem paths in CDP. No drops or downloads elsewhere,
//!   no extensions, no debugging port.
//! * The browser has its own profile and downloads in Tessera's data folder
//!   (out of reach of every machine action, never the user's own browser
//!   profiles), no desktop session (no display, clipboard, keyring or
//!   session bus) and, on Linux, a Landlock domain: it reads the system and
//!   writes only its own folders, never the home folder. Chromium's own
//!   sandbox stays on, and it listens on no port.
//! * Agents' actions arrive as `Tessera.act` first: this machine checks who
//!   acts (its own "who can use it", the chat's mode) and logs it, as it
//!   does for commands.
//! * It keeps at most [`MAX_TABS`] tabs, and stops when unused for a while
//!   or when it uses too much memory.

#![cfg_attr(not(any(unix, windows)), allow(dead_code))]

#[cfg(unix)]
type PipeReader = tokio::net::unix::pipe::Receiver;
#[cfg(unix)]
type PipeWriter = tokio::net::unix::pipe::Sender;
#[cfg(windows)]
type PipeReader = tokio::process::ChildStdout;
#[cfg(windows)]
type PipeWriter = tokio::process::ChildStdin;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tessera_proto::{
    NodeBot, NodeBrowserDown, NodeBrowserUp, NodeLogEvent, PermissionMode, Request,
};
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[cfg(all(test, unix))]
#[path = "browser_screen_tests.rs"]
mod screen_tests;

use super::browser_queue as queue;
use super::log::{NodeLog, entry};
use super::{Link, NodeConfig};
use crate::config::Paths;

/// Tabs open at once at most (the server is told; popups come on top).
pub const MAX_TABS: usize = 8;
/// Pages at most, popups included: past this a new one is closed.
const MAX_PAGES: usize = 24;
/// Virtual screens the server may add (one per render scale).
const MAX_SCREENS: usize = 16;
/// Stop after this long without a word from the server.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// The node's own command ids (the server's count up from 1 for each start
/// of the browser; the browser takes 31-bit ids).
const OWN_IDS: u64 = 1 << 30;
/// The largest download.
const MAX_DOWNLOAD: u64 = 4 << 30;
/// Everything in the downloads folder together: a page can't fill the
/// computer's disk with one download after another.
const MAX_DOWNLOADS_TOTAL: u64 = 8 << 30;
/// The longest message the browser may send.
const MAX_MESSAGE: usize = 96 << 20;
/// Bytes of news sent to the server in one message at most, and messages
/// on their way at once.
const BATCH_BYTES: usize = 2 << 20;
const IN_FLIGHT: usize = 8;

/// No browser to run here, in plain words, with the one command that gets
/// one.
#[cfg(not(windows))]
pub const NO_BROWSER: &str = "there is no browser for web views on this computer: install Chromium or Google Chrome there, or run Tessera's installer on it (it downloads one, no sudo needed)";
#[cfg(windows)]
pub const NO_BROWSER: &str = "there is no browser for web views on this computer: install Microsoft Edge or Google Chrome there";

// ---- the allowlist --------------------------------------------------------

/// What the server may send the browser, and the state that depends on:
/// the page sessions this machine attached, the stream handles it gave
/// out, and the tabs and screens open.
#[derive(Debug, Default)]
pub struct Filter {
    /// Page sessions the browser attached (the only sessions commands may
    /// name).
    pub pages: HashSet<String>,
    choosers: HashMap<String, (i64, bool)>,
    /// Stream handles `Network.loadNetworkResource` answered with.
    streams: HashSet<String>,
    /// Commands whose answers hand out a stream handle.
    loads: HashSet<u64>,
    /// PDF streams are page-scoped and read sequentially within the byte cap.
    pdfs: HashMap<u64, String>,
    pdf_streams: HashMap<String, (String, usize)>,
    /// `Target.createTarget` calls not answered yet.
    creating: HashSet<u64>,
    screens: usize,
    pub max_tabs: usize,
}

/// A refused command: the server gets an error answer with this reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub id: Option<u64>,
    pub session: Option<String>,
    pub method: String,
    pub reason: String,
}

/// Where a command goes: the browser itself, or one of the pages.
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Browser,
    Page,
}

/// A command this machine answers itself (`Tessera.*`).
#[derive(Debug, Clone, PartialEq)]
pub enum Own {
    /// The agent is about to act (who, what): check and log it.
    Act(Value),
    /// Delete a finished download.
    DiscardDownload(String),
    AgentUpload {
        session: String,
        node: Option<i64>,
        params: Value,
    },
    ChooseFiles {
        session: String,
        node: i64,
        files: Vec<tessera_proto::Attachment>,
    },
    UploadFiles {
        session: String,
        node: i64,
        owner: String,
        action: tessera_proto::BrowserUploadAction,
    },
}

/// What to do with one message from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Pass this on to the browser.
    Pass(String),
    /// Answer it here (`id`).
    Own(u64, Own),
}

fn object(v: &Value) -> Result<&Map<String, Value>, String> {
    match v {
        Value::Null => {
            static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
            Ok(EMPTY.get_or_init(Map::new))
        }
        Value::Object(m) => Ok(m),
        _ => Err("its parameters aren't an object".into()),
    }
}

/// Only these parameters (anything else is refused).
fn only(p: &Map<String, Value>, keys: &[&str]) -> Result<(), String> {
    match p.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("the parameter `{k}` isn't allowed")),
        None => Ok(()),
    }
}

/// `key` is absent or false.
fn not_true(p: &Map<String, Value>, key: &str) -> Result<(), String> {
    match p.get(key) {
        None | Some(Value::Bool(false)) | Some(Value::Null) => Ok(()),
        _ => Err(format!("`{key}` isn't allowed")),
    }
}

/// An address pages may be opened at: http(s), `about:blank`, or an HTML
/// `data:` page. Never local files, browser pages, extensions, `blob:`,
/// `javascript:` or `view-source:`.
pub fn allowed_address(raw: &str) -> Result<(), String> {
    let s = raw.trim();
    if s == "about:blank" {
        return Ok(());
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("data:text/html") {
        return Ok(());
    }
    match url::Url::parse(s) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.has_host() => Ok(()),
        Ok(u) => Err(format!("{}: addresses can't be opened here", u.scheme())),
        Err(_) => Err("that isn't an address".into()),
    }
}

impl Filter {
    pub fn new(max_tabs: usize) -> Self {
        Self {
            max_tabs,
            ..Default::default()
        }
    }

    /// Check one message from the server.
    pub fn check(&mut self, raw: &str) -> Result<Verdict, Refused> {
        let v: Value = serde_json::from_str(raw).map_err(|_| Refused {
            id: None,
            session: None,
            method: String::new(),
            reason: "unreadable".into(),
        })?;
        let id = v.get("id").and_then(Value::as_u64);
        let method = v
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let session = match v.get("sessionId") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => Some(String::new()),
        };
        let refuse = |reason: String| Refused {
            id,
            session: session.clone(),
            method: method.clone(),
            reason,
        };
        let Some(id) = id.filter(|i| *i < OWN_IDS) else {
            return Err(refuse("a command needs an id".into()));
        };
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let p = object(&params).map_err(refuse)?;
        // This machine's own.
        match method.as_str() {
            "Tessera.act" => return Ok(Verdict::Own(id, Own::Act(params.clone()))),
            "Tessera.readUpload" | "Tessera.agentChooseFiles" => {
                let s = session
                    .as_deref()
                    .filter(|s| self.pages.contains(*s))
                    .ok_or_else(|| refuse("not a page this machine opened".into()))?;
                let send = method == "Tessera.agentChooseFiles";
                only(
                    p,
                    if send {
                        &["backendNodeId", "sources", "actor"]
                    } else {
                        &["paths", "actor"]
                    },
                )
                .map_err(refuse)?;
                if params["actor"]["action"] != "upload"
                    || params["actor"]["session"]
                        .as_str()
                        .is_none_or(str::is_empty)
                {
                    return Err(refuse("uploads need their acting chat".into()));
                }
                let (node, many) = self
                    .choosers
                    .get(s)
                    .copied()
                    .ok_or_else(|| refuse("this page has no open file chooser".into()))?;
                if send {
                    if params["backendNodeId"].as_i64() != Some(node) {
                        return Err(refuse("that file chooser closed".into()));
                    }
                    let sources: crate::browser::agent_upload::Sources =
                        serde_json::from_value(params["sources"].clone())
                            .map_err(|_| refuse("the upload sources are invalid".into()))?;
                    sources.validate(&[]).map_err(refuse)?;
                    if !many && sources.files.len() > 1 {
                        return Err(refuse("this page accepts one file".into()));
                    }
                } else {
                    let paths: Vec<String> = serde_json::from_value(params["paths"].clone())
                        .map_err(|_| refuse("give paths on this machine".into()))?;
                    if paths.is_empty() || paths.len() > 8 {
                        return Err(refuse("give 1 to 8 files".into()));
                    }
                }
                return Ok(Verdict::Own(
                    id,
                    Own::AgentUpload {
                        session: s.into(),
                        node: send.then_some(node),
                        params,
                    },
                ));
            }
            "DOM.describeNode" => {
                only(p, &["backendNodeId", "depth"]).map_err(refuse)?;
                let s = session
                    .as_deref()
                    .filter(|s| self.pages.contains(*s))
                    .ok_or_else(|| refuse("not a page this machine opened".into()))?;
                if !self
                    .choosers
                    .get(s)
                    .is_some_and(|(n, _)| params["backendNodeId"].as_i64() == Some(*n))
                    || params["depth"] != 0
                {
                    return Err(refuse(
                        "only the current file input can be described".into(),
                    ));
                }
                return Ok(Verdict::Pass(
                    json!({"id":id,"sessionId":s,"method":method,"params":params}).to_string(),
                ));
            }
            "Tessera.chooseFiles" => {
                only(p, &["backendNodeId", "files"]).map_err(refuse)?;
                let s = session
                    .as_deref()
                    .filter(|s| self.pages.contains(*s))
                    .ok_or_else(|| refuse("not a page this machine opened".into()))?;
                let (node, many) = self
                    .choosers
                    .get(s)
                    .copied()
                    .ok_or_else(|| refuse("this page has no open file chooser".into()))?;
                if p["backendNodeId"].as_i64() != Some(node) {
                    return Err(refuse("that file chooser closed".into()));
                }
                let files: Vec<tessera_proto::Attachment> =
                    serde_json::from_value(p["files"].clone())
                        .map_err(|_| refuse("only selected file bytes can be uploaded".into()))?;
                if files.len() > 8 || (!many && files.len() > 1) {
                    return Err(refuse("too many selected files for this chooser".into()));
                }
                return Ok(Verdict::Own(
                    id,
                    Own::ChooseFiles {
                        session: s.into(),
                        node,
                        files,
                    },
                ));
            }
            "Tessera.uploadFiles" => {
                only(p, &["backendNodeId", "owner", "action"]).map_err(refuse)?;
                let s = session
                    .as_deref()
                    .filter(|s| self.pages.contains(*s))
                    .ok_or_else(|| refuse("not a page this machine opened".into()))?;
                let owner = p["owner"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 200)
                    .ok_or_else(|| refuse("this upload needs its picker".into()))?;
                let action: tessera_proto::BrowserUploadAction =
                    serde_json::from_value(p["action"].clone())
                        .map_err(|_| refuse("only selected file chunks can be uploaded".into()))?;
                let (node, many) = if matches!(
                    action,
                    tessera_proto::BrowserUploadAction::Cancel { .. }
                        | tessera_proto::BrowserUploadAction::Finish { .. }
                ) {
                    (p["backendNodeId"].as_i64().unwrap_or(0), true)
                } else {
                    let (node, many) = self
                        .choosers
                        .get(s)
                        .copied()
                        .ok_or_else(|| refuse("this page has no open file chooser".into()))?;
                    if p["backendNodeId"].as_i64() != Some(node) {
                        return Err(refuse("that file chooser closed".into()));
                    }
                    (node, many)
                };
                if let tessera_proto::BrowserUploadAction::Start { ref files } = action
                    && (files.len() > 8 || (!many && files.len() > 1))
                {
                    return Err(refuse("too many selected files for this chooser".into()));
                }
                return Ok(Verdict::Own(
                    id,
                    Own::UploadFiles {
                        session: s.into(),
                        node,
                        owner: format!("{s}:{owner}"),
                        action,
                    },
                ));
            }
            "Tessera.discardDownload" => {
                let guid = p.get("guid").and_then(Value::as_str).unwrap_or_default();
                return Ok(Verdict::Own(id, Own::DiscardDownload(guid.to_string())));
            }
            _ => {}
        }
        if let Some((owner, _)) = p
            .get("handle")
            .and_then(Value::as_str)
            .and_then(|h| self.pdf_streams.get(h))
            && session.as_deref() != Some(owner.as_str())
        {
            return Err(refuse("not this page's PDF stream".into()));
        }
        let level = self.rule(&method, p, id).map_err(refuse)?;
        match (level, &session) {
            (Level::Browser, None) => {}
            (Level::Page, Some(s)) if self.pages.contains(s) => {}
            (Level::Page, Some(_)) => return Err(refuse("not a page this machine opened".into())),
            (Level::Page, None) => return Err(refuse("it needs a page".into())),
            (Level::Browser, Some(_)) => return Err(refuse("not for a page".into())),
        }
        if method == "Page.printToPDF" {
            self.pdfs.insert(id, session.clone().unwrap());
        }
        if matches!(method.as_str(), "IO.read" | "IO.close") {
            let handle = p["handle"].as_str().unwrap_or_default();
            if let Some((_, read)) = self.pdf_streams.get_mut(handle) {
                if method == "IO.read" {
                    *read += p["size"].as_u64().unwrap() as usize;
                } else {
                    self.pdf_streams.remove(handle);
                }
            }
        }
        // Passed on as it was checked: only these four fields.
        let mut out = json!({"id": id, "method": method});
        if !params.is_null() {
            out["params"] = params;
        }
        if let Some(s) = session {
            out["sessionId"] = Value::String(s);
        }
        Ok(Verdict::Pass(out.to_string()))
    }

    /// The allowlist: where a command goes, after checking its
    /// parameters. Anything not listed is refused.
    fn rule(&mut self, method: &str, p: &Map<String, Value>, id: u64) -> Result<Level, String> {
        use Level::*;
        Ok(match method {
            // ---- the browser --------------------------------------------
            "Browser.getVersion" => Browser,
            "Browser.getWindowForTarget" => {
                only(p, &["targetId"])?;
                Browser
            }
            "Browser.setWindowBounds" => {
                only(p, &["windowId", "bounds"])?;
                Browser
            }
            "Browser.cancelDownload" => {
                only(p, &["guid"])?;
                Browser
            }
            "Target.createTarget" => {
                only(
                    p,
                    &[
                        "url",
                        "background",
                        "newWindow",
                        "left",
                        "top",
                        "width",
                        "height",
                    ],
                )?;
                if p.get("url").and_then(Value::as_str) != Some("about:blank") {
                    return Err("new tabs open blank".into());
                }
                if self.pages.len() + self.creating.len() >= self.max_tabs {
                    return Err(format!(
                        "this machine keeps at most {} tabs open; close one first",
                        self.max_tabs
                    ));
                }
                self.creating.insert(id);
                Browser
            }
            "Target.closeTarget" | "Target.getTargetInfo" => {
                only(p, &["targetId"])?;
                Browser
            }
            "Target.detachFromTarget" => {
                only(p, &["sessionId"])?;
                Browser
            }
            "Emulation.getScreenInfos" => Browser,
            "Emulation.addScreen" => {
                only(
                    p,
                    &[
                        "left",
                        "top",
                        "width",
                        "height",
                        "devicePixelRatio",
                        "label",
                        "workAreaInsets",
                        "rotation",
                        "colorDepth",
                        "isInternal",
                    ],
                )?;
                if self.screens >= MAX_SCREENS {
                    return Err("too many screens".into());
                }
                self.screens += 1;
                Browser
            }
            "IO.read" | "IO.close" => {
                only(p, &["handle", "offset", "size"])?;
                let h = p.get("handle").and_then(Value::as_str).unwrap_or_default();
                if let Some((_, read)) = self.pdf_streams.get(h) {
                    only(
                        p,
                        if method == "IO.close" {
                            &["handle"]
                        } else {
                            &["handle", "size"]
                        },
                    )?;
                    if method == "IO.read" {
                        let size = p["size"]
                            .as_u64()
                            .filter(|s| {
                                *s > 0 && *s <= crate::browser::page_actions::PDF_CHUNK as u64
                            })
                            .ok_or("PDF reads need a bounded size")?
                            as usize;
                        if read.saturating_add(size) > crate::browser::page_actions::PDF_LIMIT + 1 {
                            return Err("this PDF is larger than 16 MB".into());
                        }
                    }
                    return Ok(Page);
                }
                if !self.streams.contains(h) {
                    return Err("not a stream this machine handed out".into());
                }
                if method == "IO.close" {
                    self.streams.remove(h);
                }
                Browser
            }
            // ---- pages ----------------------------------------------------
            "Page.enable" => {
                only(p, &["enableFileChooserOpenedEvent"])?;
                Page
            }
            "Page.navigate" => {
                only(
                    p,
                    &[
                        "url",
                        "referrer",
                        "transitionType",
                        "frameId",
                        "referrerPolicy",
                    ],
                )?;
                allowed_address(p.get("url").and_then(Value::as_str).unwrap_or_default())?;
                Page
            }
            "Page.reload" => {
                only(p, &["ignoreCache", "loaderId"])?;
                Page
            }
            "Page.getFrameTree"
            | "Page.stopLoading"
            | "Page.bringToFront"
            | "Page.getNavigationHistory"
            | "Page.getLayoutMetrics"
            | "Page.stopScreencast"
            | "Runtime.enable"
            | "Runtime.disable"
            | "Runtime.runIfWaitingForDebugger"
            | "Emulation.clearDeviceMetricsOverride" => {
                only(p, &[])?;
                Page
            }
            "Page.navigateToHistoryEntry" => {
                only(p, &["entryId"])?;
                Page
            }
            "Page.handleJavaScriptDialog" => {
                only(p, &["accept", "promptText"])?;
                Page
            }
            "Page.printToPDF" => {
                only(p, &["printBackground", "preferCSSPageSize", "transferMode"])?;
                if p.get("transferMode").and_then(Value::as_str) != Some("ReturnAsStream") {
                    return Err("PDF export returns selected page bytes only".into());
                }
                Page
            }
            "Page.captureScreenshot" => {
                only(
                    p,
                    &[
                        "format",
                        "quality",
                        "clip",
                        "fromSurface",
                        "captureBeyondViewport",
                        "optimizeForSpeed",
                    ],
                )?;
                Page
            }
            "Page.startScreencast" => {
                only(
                    p,
                    &[
                        "format",
                        "quality",
                        "maxWidth",
                        "maxHeight",
                        "everyNthFrame",
                    ],
                )?;
                Page
            }
            "Page.screencastFrameAck" => {
                only(p, &["sessionId"])?;
                Page
            }
            "Page.createIsolatedWorld" => {
                only(p, &["frameId", "worldName", "grantUniveralAccess"])?;
                not_true(p, "grantUniveralAccess")?;
                Page
            }
            "Page.addScriptToEvaluateOnNewDocument" => {
                only(
                    p,
                    &[
                        "source",
                        "worldName",
                        "includeCommandLineAPI",
                        "runImmediately",
                    ],
                )?;
                not_true(p, "includeCommandLineAPI")?;
                Page
            }
            "Page.setInterceptFileChooserDialog" => {
                only(p, &["enabled", "cancel"])?;
                Page
            }
            "Page.setWebLifecycleState" => {
                only(p, &["state"])?;
                Page
            }
            "Runtime.addBinding" => {
                only(p, &["name", "executionContextName", "executionContextId"])?;
                Page
            }
            "Runtime.evaluate" => {
                only(
                    p,
                    &[
                        "expression",
                        "contextId",
                        "uniqueContextId",
                        "returnByValue",
                        "awaitPromise",
                        "timeout",
                        "silent",
                        "userGesture",
                        "generatePreview",
                        "objectGroup",
                        "throwOnSideEffect",
                        "includeCommandLineAPI",
                    ],
                )?;
                not_true(p, "includeCommandLineAPI")?;
                Page
            }
            "Emulation.setDeviceMetricsOverride"
            | "Emulation.setUserAgentOverride"
            | "Emulation.setTouchEmulationEnabled"
            | "Emulation.setScrollbarsHidden"
            | "Emulation.setFocusEmulationEnabled" => Page,
            "Input.dispatchMouseEvent" | "Input.dispatchTouchEvent" | "Input.insertText" => Page,
            "Input.dispatchKeyEvent" => {
                // No editing commands (`paste` would read this computer's
                // clipboard into the page).
                if p.contains_key("commands") {
                    return Err("editing commands aren't allowed".into());
                }
                Page
            }
            "Fetch.enable" => {
                only(p, &["patterns", "handleAuthRequests"])?;
                not_true(p, "handleAuthRequests")?;
                Page
            }
            "Fetch.disable" => {
                only(p, &[])?;
                Page
            }
            "Fetch.continueRequest" => {
                // No URL, headers, body or redirect overrides: only release
                // the exact request held during this known page's setup.
                only(p, &["requestId"])?;
                if !p
                    .get("requestId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                {
                    return Err("the held request needs its id".into());
                }
                Page
            }
            "Fetch.fulfillRequest" => {
                // Only the fixed inert bootstrap document, not arbitrary page
                // content, cookies, redirects or synthetic network responses.
                let id = p
                    .get("requestId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or("the held document needs its request id")?;
                if Value::Object(p.clone()) != crate::browser::cdp::inert_document_response(id) {
                    return Err("only an empty, uncacheable setup document is allowed".into());
                }
                Page
            }
            "Fetch.failRequest" => {
                only(p, &["requestId", "errorReason"])?;
                Page
            }
            "Network.loadNetworkResource" => {
                only(p, &["frameId", "url", "options"])?;
                let url = p.get("url").and_then(Value::as_str).unwrap_or_default();
                match url::Url::parse(url) {
                    Ok(u) if matches!(u.scheme(), "http" | "https") => {}
                    _ => return Err("only web addresses can be loaded".into()),
                }
                let creds = p.get("options").and_then(|o| o.get("includeCredentials"));
                if !matches!(creds, Some(Value::Bool(false))) {
                    return Err("loads go without credentials".into());
                }
                self.loads.insert(id);
                Page
            }
            m => {
                return Err(format!(
                    "{} isn't allowed on a machine",
                    if m.is_empty() { "that" } else { m }
                ));
            }
        })
    }

    /// Learn from what the browser sends (`v`, already parsed): page
    /// sessions it attached or lost, stream handles it handed out, tabs it
    /// made. Returns a page to close at once (too many open).
    pub fn learn(&mut self, v: &Value) -> Option<String> {
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            if self.loads.remove(&id)
                && let Some(h) = v["result"]["resource"]["stream"].as_str()
            {
                self.streams.insert(h.to_string());
            }
            if let Some(session) = self.pdfs.remove(&id)
                && self.pages.contains(&session)
                && let Some(handle) = v["result"]["stream"].as_str()
            {
                self.pdf_streams.insert(handle.into(), (session, 0));
            }
            self.creating.remove(&id);
            return None;
        }
        let p = &v["params"];
        let target = || p["targetInfo"]["targetId"].as_str().map(str::to_string);
        // A page the browser itself opened on one of its own pages (a
        // keyboard shortcut for settings, history…): closed at once.
        let inside = || {
            let url = p["targetInfo"]["url"].as_str().unwrap_or_default();
            !url.is_empty() && !url.starts_with("chrome-error:") && allowed_address(url).is_err()
        };
        match v.get("method").and_then(Value::as_str).unwrap_or_default() {
            "Target.attachedToTarget" if p["targetInfo"]["type"] == "page" => {
                let s = p["sessionId"].as_str().unwrap_or_default().to_string();
                if self.pages.len() >= MAX_PAGES || inside() {
                    return target();
                }
                if !s.is_empty() {
                    self.pages.insert(s);
                }
                None
            }
            "Target.targetInfoChanged" if p["targetInfo"]["type"] == "page" && inside() => target(),
            "Target.detachedFromTarget" => {
                if let Some(s) = p["sessionId"].as_str() {
                    self.pages.remove(s);
                    self.choosers.remove(s);
                    self.pdf_streams.retain(|_, (owner, _)| owner != s);
                    self.pdfs.retain(|_, owner| owner != s);
                }
                None
            }
            "Page.fileChooserOpened" => {
                if let (Some(s), Some(node)) = (
                    v["sessionId"].as_str(),
                    p["backendNodeId"].as_i64().filter(|n| *n > 0),
                ) && self.pages.contains(s)
                {
                    self.choosers
                        .insert(s.into(), (node, p["mode"] == "selectMultiple"));
                }
                None
            }
            "Page.frameNavigated" | "Page.frameDetached" => {
                if let Some(s) = v["sessionId"].as_str() {
                    self.choosers.remove(s);
                }
                None
            }
            _ => None,
        }
    }
}

/// The error answer for a refused command.
pub fn refusal(r: &Refused) -> Option<String> {
    let id = r.id?;
    let mut out = json!({
        "id": id,
        "error": {"code": -32000, "message": format!("refused by this machine: {}", r.reason)},
    });
    if let Some(s) = &r.session {
        out["sessionId"] = Value::String(s.clone());
    }
    Some(out.to_string())
}

// ---- the agent's actions --------------------------------------------------

/// Actions that change something on a page (a read-only chat may not).
fn changes(action: &str) -> bool {
    matches!(
        action,
        "upload"
            | "click"
            | "type"
            | "press"
            | "select"
            | "dialog"
            | "navigate"
            | "open"
            | "back"
            | "forward"
            | "reload"
    )
}

/// May the agent act as `act` says (`Tessera.act`)? `Err` says why not.
pub fn may_act(config: &NodeConfig, paused: bool, act: &Value) -> Result<(), String> {
    if !config.enabled {
        return Err("this machine is turned off".into());
    }
    if config.scope.stopped {
        return Err(super::STOPPED_HERE.into());
    }
    super::stopbar::controls_ready()?;
    // Asked for by a turn from before the user's latest Stop here: never
    // after it (`stops` is its count; older servers send none).
    if act
        .get("stops")
        .and_then(Value::as_u64)
        .is_some_and(|s| s != u64::from(config.scope.stops))
    {
        return Err(super::ASKED_BEFORE_STOP.into());
    }
    if paused {
        return Err("this machine is paused from another device".into());
    }
    if !config.scope.webviews {
        return Err("web views are off on this machine".into());
    }
    let bot: Option<NodeBot> = serde_json::from_value(act["bot"].clone()).ok();
    super::may_use(
        &config.scope,
        bot.as_ref().map(|b| (b.id.as_str(), b.handle.as_str())),
    )
    .map_err(|why| format!("{why}; who may use this machine is set on this computer"))?;
    let mode: PermissionMode =
        serde_json::from_value(act["mode"].clone()).unwrap_or(PermissionMode::ReadOnly);
    let action = act["action"].as_str().unwrap_or_default();
    if mode == PermissionMode::ReadOnly && changes(action) {
        return Err("the chat is read-only, so this machine changes nothing for it".into());
    }
    Ok(())
}

// ---- the browser ------------------------------------------------------------

/// What the node knows when it handles the server's messages.
pub struct Ctx<'a> {
    pub config: &'a NodeConfig,
    pub paused: bool,
}

pub struct NodeBrowser(Arc<Inner>);

struct Inner {
    paths: Paths,
    log: NodeLog,
    run: Mutex<Option<Run>>,
    /// Graceful shutdown remains revocable after its Run leaves `run`.
    stopping: Mutex<HashMap<u64, CancellationToken>>,
    /// Held for the child's whole lifetime; no shared-profile reuse mid-exit.
    profile_gate: Arc<tokio::sync::Mutex<()>>,
    /// News for the server, in order.
    up: Mutex<Upstream>,
    /// Batches sent on this connection (the server expects 1 first).
    seq: Arc<AtomicU64>,
    /// Refusals logged this minute (a misbehaving server can't flood the log).
    logged: Mutex<(Instant, u32)>,
    /// The window on this computer's screen, while it is open.
    screen: Mutex<Option<Screen>>,
    /// Pending launches are admitted before detaching and revoked by Stop.
    screen_launch: Mutex<ScreenLaunches>,
}

/// "Show on <this computer>'s screen": an ordinary, visible browser window
/// with the web views' own profile (the same sign-ins), so the user can
/// finish there what a web view can't (a site's verification check).
/// Nobody drives it: no debugging pipe, nothing automated. The browser for
/// web views waits meanwhile (they share the profile).
struct Screen {
    #[cfg(unix)]
    pid: Option<u32>,
    /// Close it (it is asked to quit, so it saves what changed).
    close: CancellationToken,
    /// Closed on request (the request answers), not by the user there.
    asked: Arc<std::sync::atomic::AtomicBool>,
    /// True once it has exited.
    gone: tokio::sync::watch::Receiver<bool>,
}

#[derive(Default)]
struct ScreenLaunches {
    next: u64,
    pending: Option<ScreenLaunch>,
}

#[derive(Clone)]
struct ScreenLaunch {
    id: u64,
    stops: u32,
    cancel: CancellationToken,
}

#[cfg(unix)]
impl ScreenLaunch {
    async fn wait<T>(&self, work: impl std::future::Future<Output = T>) -> Result<T, String> {
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(STOPPED_SINCE_START.into()),
            value = work => Ok(value),
        }
    }
}

/// Covers the spawn-to-registration gap, including a dropped launch future.
#[cfg(unix)]
struct ScreenChild {
    child: Option<tokio::process::Child>,
    pgid: Option<u32>,
    lease: Option<crate::browser::process::ExitLease>,
}
#[cfg(unix)]
impl ScreenChild {
    fn take_cleanup(
        &mut self,
    ) -> Option<(
        tokio::process::Child,
        Option<crate::browser::process::ExitLease>,
    )> {
        // Clear the stored id before any wait or Drop can signal it again.
        kill_group(self.pgid.take());
        let mut child = self.child.take()?;
        let _ = child.start_kill();
        Some((child, self.lease.take()))
    }
    async fn cleanup(&mut self) -> Option<std::process::ExitStatus> {
        let (child, lease) = self.take_cleanup()?;
        crate::browser::process::hold_until_exit(
            crate::browser::process::confirmed_exit(child),
            lease,
            Duration::from_secs(2),
        )
        .await
    }
}
#[cfg(unix)]
impl Drop for ScreenChild {
    fn drop(&mut self) {
        if let Some((child, lease)) = self.take_cleanup() {
            tokio::spawn(async move {
                let _ = crate::browser::process::hold_until_exit(
                    crate::browser::process::confirmed_exit(child),
                    lease,
                    Duration::from_secs(2),
                )
                .await;
            });
        }
    }
}

/// News and byte leases belong to just one server connection.
struct Upstream {
    tx: queue::Sender<NodeBrowserUp>,
    rx: Option<queue::Receiver<NodeBrowserUp>>,
    stop: CancellationToken,
    /// Actual disconnect, unlike overflow, ends the supervisor/retries.
    connection: CancellationToken,
    /// Starts replace one another on the server. Retain its latest waiter
    /// while overflow reporting is blocked; never an unbounded error queue.
    rejected: Option<u64>,
    failed: Option<u64>,
    forwarding: bool,
}
impl Upstream {
    fn new() -> Self {
        let stop = CancellationToken::new();
        let (tx, rx) = queue::channel(64 << 20, stop.clone());
        Self {
            tx,
            rx: Some(rx),
            stop,
            connection: CancellationToken::new(),
            rejected: None,
            failed: None,
            forwarding: false,
        }
    }
}

/// One start of the browser.
struct Run {
    run: u64,
    /// Checked commands for the browser, in order.
    tx: queue::Sender<Vec<u8>>,
    filter: Filter,
    own: Arc<OwnCalls>,
    stop: CancellationToken,
    /// Stopped on purpose (no "stopped unexpectedly" then).
    asked: Arc<std::sync::atomic::AtomicBool>,
    last_use: Arc<Mutex<Instant>>,
    downloads: PathBuf,
    /// Downloads: suggested names, and where finished ones were kept.
    names: HashMap<String, String>,
    saved: HashMap<String, PathBuf>,
    /// Bytes of the files in the downloads folder, and of each download
    /// still coming in.
    kept: u64,
    receiving: HashMap<String, u64>,
    /// The browser's process (its group: every process it starts).
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<Arc<super::browser_windows::Job>>,
    /// The count of the user's Stops here when it started: after a later
    /// Stop nothing more reaches it (it closes at the machine's next look).
    stops: u32,
    uploads: crate::browser::upload::Uploads,
    upload_finishes: HashMap<u64, (String, String, String, i64)>,
}

impl Run {
    /// Planned idle/memory stop, not a revocation: let Chrome flush the
    /// profile's persistent cookies/sign-ins before the kill fallback.
    async fn save_and_stop(self) {
        self.asked.store(true, Ordering::SeqCst);
        tokio::select! {
            biased;
            _ = self.stop.cancelled() => {},
            _ = async {
                let _ = tokio::time::timeout(Duration::from_secs(2),
                    self.own.call(&self.tx, "Browser.close", json!({}))).await;
                let _ = tokio::time::timeout(Duration::from_secs(5), self.stop.cancelled()).await;
            } => {},
        }
        self.stop.cancel();
        #[cfg(unix)]
        wait_gone(self.pid, Duration::from_secs(3)).await;
    }
}

/// The user's Stop since a browser started (or now): its commands are refused.
const STOPPED_SINCE_START: &str =
    "the user stopped Tessera on this computer, so this browser closes; try again in a moment";

/// The node's own commands to the browser, and their answers.
#[derive(Default)]
struct OwnCalls {
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
}

impl OwnCalls {
    fn send(
        &self,
        tx: &queue::Sender<Vec<u8>>,
        method: &str,
        params: Value,
    ) -> oneshot::Receiver<Result<Value, String>> {
        let id = OWN_IDS + self.next.fetch_add(1, Ordering::Relaxed);
        let (otx, orx) = oneshot::channel();
        self.waiting.lock().insert(id, otx);
        let msg = json!({"id": id, "method": method, "params": params});
        if tx.send(msg.to_string().into_bytes()).is_err() {
            self.waiting.lock().remove(&id);
        }
        orx
    }

    /// Send one without waiting for its answer.
    fn fire(&self, tx: &queue::Sender<Vec<u8>>, method: &str, params: Value) {
        let id = OWN_IDS + self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"id": id, "method": method, "params": params});
        let _ = tx.send(msg.to_string().into_bytes());
    }

    async fn call(
        &self,
        tx: &queue::Sender<Vec<u8>>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let rx = self.send(tx, method, params);
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the browser closed".into()),
            Err(_) => Err(format!("the browser didn't answer `{method}` in time")),
        }
    }

    /// An answer to one of ours: taken (true), or not ours.
    fn answer(&self, v: &Value) -> bool {
        let Some(id) = v
            .get("id")
            .and_then(Value::as_u64)
            .filter(|i| *i >= OWN_IDS)
        else {
            return false;
        };
        if let Some(tx) = self.waiting.lock().remove(&id) {
            let r = match v.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("failed").to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        true
    }
}

/// Create a folder only its owner can use.
fn private_dir(d: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        crate::guard::windows::private_browser_dir(d).map(|held| held.hold())
    }
    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

/// Where the web view browser keeps its things on this computer.
pub fn dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("web-browser")
}

impl NodeBrowser {
    pub fn new(paths: Paths) -> Self {
        Self(Arc::new(Inner {
            log: NodeLog::new(super::log_file(&paths)),
            paths,
            run: Mutex::new(None),
            stopping: Mutex::new(HashMap::new()),
            profile_gate: Arc::new(tokio::sync::Mutex::new(())),
            up: Mutex::new(Upstream::new()),
            seq: Arc::new(AtomicU64::new(0)),
            logged: Mutex::new((Instant::now(), 0)),
            screen: Mutex::new(None),
            screen_launch: Mutex::new(ScreenLaunches::default()),
        }))
    }

    /// Whether the browser runs now.
    pub fn running(&self) -> bool {
        self.0.run.lock().is_some()
    }

    /// A new connection: news counts from 1 again (the browser stopped
    /// when the last one was lost).
    pub fn connected(&self) {
        let mut up = self.0.up.lock();
        up.connection.cancel();
        up.stop.cancel();
        *up = Upstream::new();
        self.0.seq.store(0, Ordering::SeqCst);
    }

    /// Drop queued/in-flight browser news without waiting for a blocked link.
    pub fn disconnected(&self, why: &str) {
        {
            let up = self.0.up.lock();
            up.connection.cancel();
            up.stop.cancel();
        }
        self.stop(why);
    }

    /// Stop the browser (switched off, paused, revoked, disconnected,
    /// turned off), saying why in the log. A window on the screen closes too.
    pub fn stop(&self, why: &str) {
        let mut launches = self.0.screen_launch.lock();
        if let Some(p) = launches.pending.take() {
            p.cancel.cancel();
        }
        if let Some(s) = self.0.screen.lock().take() {
            s.asked.store(true, Ordering::SeqCst);
            s.close.cancel();
            let _ = self.0.send_up(NodeBrowserUp::Screen {
                shown: false,
                error: None,
            });
        }
        drop(launches);
        let run = {
            let mut cur = self.0.run.lock();
            for token in self.0.stopping.lock().values() {
                token.cancel();
            }
            cur.take()
        };
        if let Some(r) = run {
            r.asked.store(true, Ordering::SeqCst);
            r.stop.cancel();
            self.0.log_line(
                NodeLogEvent::Done,
                format!("web views: the browser stopped ({why})"),
            );
            let _ = self.0.send_up(NodeBrowserUp::Stopped {
                run: r.run,
                reason: Some(why.to_string()),
                crashed: false,
            });
        }
    }

    /// The server's messages for the browser, in order.
    pub fn handle(&self, link: &Arc<dyn Link>, msgs: Vec<NodeBrowserDown>, ctx: &Ctx) {
        self.0.clone().forward_with(link.clone());
        for m in msgs {
            match m {
                NodeBrowserDown::Start { run, scale } => self.start(run, scale, ctx),
                NodeBrowserDown::Stop { run } => {
                    let r = {
                        let mut cur = self.0.run.lock();
                        if let Some(token) = self.0.stopping.lock().get(&run) {
                            token.cancel();
                        }
                        if cur.as_ref().is_some_and(|r| r.run == run) {
                            cur.take()
                        } else {
                            None
                        }
                    };
                    if let Some(r) = r {
                        r.asked.store(true, Ordering::SeqCst);
                        r.stop.cancel();
                        let _ = self.0.send_up(NodeBrowserUp::Stopped {
                            run,
                            reason: None,
                            crashed: false,
                        });
                    }
                }
                NodeBrowserDown::Cdp { run, messages } => self.commands(run, messages, ctx),
                NodeBrowserDown::Screen { url } => self.screen(url, ctx),
            }
        }
    }

    /// Show `url` on this computer's screen (`None`: close that window),
    /// answering `screen`.
    fn screen(&self, url: Option<String>, ctx: &Ctx) {
        let inner = self.0.clone();
        let Some(url) = url else {
            inner.cancel_screen_launch();
            tokio::spawn(async move {
                inner.close_screen().await;
                let _ = inner.send_up(NodeBrowserUp::Screen {
                    shown: false,
                    error: None,
                });
            });
            return;
        };
        let refused = if !ctx.config.enabled {
            Some("this machine is turned off".to_string())
        } else if ctx.config.scope.stopped {
            Some(super::STOPPED_HERE.into())
        } else if ctx.paused {
            Some("this machine is paused from another device".into())
        } else if !ctx.config.scope.webviews {
            Some("web views are off on this machine".into())
        } else if !matches!(url::Url::parse(&url), Ok(u) if matches!(u.scheme(), "http" | "https") && u.has_host())
        {
            Some("only web pages (http or https) can be shown".into())
        } else {
            None
        };
        if let Some(error) = refused {
            self.0.log_line(
                NodeLogEvent::Refused,
                format!("web views: didn't show a page on this computer's screen: {error}"),
            );
            let _ = self.0.send_up(NodeBrowserUp::Screen {
                shown: false,
                error: Some(error),
            });
            return;
        }
        let launch = match inner.begin_screen(ctx.config.scope.stops) {
            Ok(launch) => launch,
            Err(error) => {
                let _ = inner.send_up(NodeBrowserUp::Screen {
                    shown: false,
                    error: Some(error),
                });
                return;
            }
        };
        tokio::spawn(async move {
            let shown = inner.clone().open_screen(&url, &launch).await;
            let mut pending = inner.screen_launch.lock();
            if pending.pending.as_ref().is_some_and(|p| p.id == launch.id) {
                pending.pending = None;
            }
            let stopped = launch.cancel.is_cancelled();
            let (shown, error) = match shown {
                Ok(()) if !stopped => (true, None),
                Ok(()) => (false, Some(STOPPED_SINCE_START.into())),
                Err(e) => {
                    inner.log_line(
                        NodeLogEvent::Failed,
                        format!("web views: couldn't show the page on this computer's screen: {e}"),
                    );
                    (false, Some(e))
                }
            };
            let _ = inner.send_up(NodeBrowserUp::Screen { shown, error });
            drop(pending);
        });
    }

    fn start(&self, run: u64, scale: Option<f64>, ctx: &Ctx) {
        {
            let mut up = self.0.up.lock();
            if up.stop.is_cancelled() {
                up.rejected = Some(run);
                return;
            }
        }
        let fail = |error: String| {
            self.0.log_line(
                NodeLogEvent::Refused,
                format!("web views: the browser didn't start: {error}"),
            );
            let up = self.0.up.lock();
            // A refusal is structural, not inferred from human text.
            let _ = up.tx.send(NodeBrowserUp::Stopped {
                run,
                reason: Some(error.clone()),
                crashed: false,
            });
            let _ = up.tx.send(NodeBrowserUp::Failed { run, error });
        };
        if !ctx.config.enabled {
            return fail("this machine is turned off".into());
        }
        if ctx.config.scope.stopped {
            return fail(super::STOPPED_HERE.into());
        }
        if ctx.paused {
            return fail("this machine is paused from another device".into());
        }
        if !ctx.config.scope.webviews {
            return fail("web views are off on this machine; only this computer can turn them on (Settings → Devices → This computer, or `tessera node preset … --webviews on`)".into());
        }
        if self.0.screen_launch.lock().pending.is_some() || self.0.screen.lock().is_some() {
            return fail("the page is open in a window on this computer's screen; close that window there first (or choose Back to the tile)".into());
        }
        // A new start replaces the old one.
        if let Some(old) = self.0.run.lock().take() {
            old.asked.store(true, Ordering::SeqCst);
            old.stop.cancel();
        }
        let Some(bin) = crate::browser::process::find(None) else {
            self.0.log_line(
                NodeLogEvent::Failed,
                "web views: no browser installed".into(),
            );
            let _ = self.0.send_up(NodeBrowserUp::Failed {
                run,
                error: NO_BROWSER.into(),
            });
            return;
        };
        let base = dir(&self.0.paths);
        let stop = self.0.up.lock().stop.child_token();
        let (tx, rx) = queue::channel(8 << 20, stop.clone());
        let r = Run {
            run,
            tx: tx.clone(),
            filter: Filter::new(MAX_TABS),
            own: Arc::new(OwnCalls::default()),
            stop,
            asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_use: Arc::new(Mutex::new(Instant::now())),
            upload_finishes: HashMap::new(),
            uploads: crate::browser::upload::Uploads::new(base.join("uploads")),
            downloads: base.join("downloads"),
            names: HashMap::new(),
            saved: HashMap::new(),
            kept: folder_bytes(&base.join("downloads")),
            receiving: HashMap::new(),
            pid: None,
            #[cfg(windows)]
            job: None,
            stops: ctx.config.scope.stops,
        };
        let (own, stop, asked, last_use) = (
            r.own.clone(),
            r.stop.clone(),
            r.asked.clone(),
            r.last_use.clone(),
        );
        *self.0.run.lock() = Some(r);
        let inner = self.0.clone();
        tokio::spawn(async move {
            let started = inner
                .clone()
                .launch(
                    run,
                    &bin,
                    &base,
                    scale,
                    tx,
                    rx,
                    own,
                    stop.clone(),
                    asked.clone(),
                )
                .await;
            match started {
                Ok((product, key)) => {
                    *last_use.lock() = Instant::now();
                    inner.log_line(
                        NodeLogEvent::Done,
                        format!("web views: the browser started ({product})"),
                    );
                    let _ = inner.send_up(NodeBrowserUp::Started {
                        run,
                        product,
                        speed_key: key,
                        max_tabs: MAX_TABS as u32,
                    });
                    tokio::spawn(inner.watch(run, stop, last_use));
                }
                Err(error) => {
                    {
                        let mut cur = inner.run.lock();
                        if cur.as_ref().is_some_and(|r| r.run == run) {
                            *cur = None;
                        }
                    }
                    stop.cancel();
                    inner.log_line(
                        NodeLogEvent::Refused,
                        format!("web views: the browser didn't start: {error}"),
                    );
                    if asked.load(Ordering::SeqCst) {
                        let _ = inner.send_up(NodeBrowserUp::Stopped {
                            run,
                            reason: Some(error.clone()),
                            crashed: false,
                        });
                    }
                    let _ = inner.send_up(NodeBrowserUp::Failed { run, error });
                }
            }
        });
    }

    /// The server's commands: checked, then passed on (or answered here).
    fn commands(&self, run: u64, messages: Vec<String>, ctx: &Ctx) {
        let mut answers = Vec::new();
        let mut refusals = Vec::new();
        {
            let mut cur = self.0.run.lock();
            let Some(r) = cur.as_mut().filter(|r| r.run == run) else {
                return;
            };
            *r.last_use.lock() = Instant::now();
            // Stopped here now, or since this browser started (also a Stop
            // saved but not yet taken in): nothing more reaches it, whoever
            // asks, whatever was allowed before.
            let stale = ctx.config.scope.stopped || ctx.config.scope.stops != r.stops;
            for raw in messages {
                match r.filter.check(&raw) {
                    Ok(Verdict::Pass(msg)) if stale => {
                        if let Some(id) = serde_json::from_str::<Value>(&msg)
                            .ok()
                            .and_then(|v| v.get("id").cloned())
                        {
                            answers.push(
                                json!({"id": id, "error": {"code": -32000, "message": STOPPED_SINCE_START}})
                                    .to_string(),
                            );
                        }
                    }
                    Ok(Verdict::Pass(msg)) => {
                        let _ = r.tx.send(msg.into_bytes());
                    }
                    Ok(Verdict::Own(id, own)) => {
                        let answer = match own {
                            Own::Act(act) => {
                                let ok = if stale {
                                    Err(STOPPED_SINCE_START.to_string())
                                } else {
                                    may_act(ctx.config, ctx.paused, &act)
                                };
                                self.0.log_act(&act, ok.as_ref().err());
                                ok.map(|_| json!({}))
                            }
                            Own::AgentUpload {
                                session,
                                node,
                                params,
                            } => {
                                let actor = &params["actor"];
                                let checked = if stale {
                                    Err(STOPPED_SINCE_START.to_string())
                                } else {
                                    may_act(ctx.config, ctx.paused, actor)
                                };
                                let result = checked.and_then(|_| {
                                    use crate::browser::agent_upload::{self, Sources};
                                    if actor["uploadAsk"] != json!(ctx.config.scope.ask) {
                                        return Err("This machine's upload confirmation settings changed. Ask again.".into());
                                    }
                                    let home = super::home();
                                    let access = super::exec::Access::new(&self.0.paths, &ctx.config.scope, home.as_deref());
                                    let protected = super::protected(&self.0.paths);
                                    let check = |p: &Path| {
                                        agent_upload::source_path(p, &protected, home.as_deref())?;
                                        access.check(p, false)
                                    };
                                    if let Some(node) = node {
                                        let sources: Sources = serde_json::from_value(params["sources"].clone()).map_err(|_| "Invalid upload sources.")?;
                                        for path in &sources.paths {
                                            let resolved = access.resolve(path, false)?;
                                            if resolved != Path::new(path) {
                                                return Err("An upload source changed location. Nothing was sent.".into());
                                            }
                                            check(&resolved)?;
                                        }
                                        sources.validate(&[])?;
                                        let paths = r.uploads.stage_agent(&sources.files)?;
                                        let msg = json!({"id":id,"sessionId":session,"method":"DOM.setFileInputFiles","params":{"backendNodeId":node,"files":paths}});
                                        r.tx.send(msg.to_string().into_bytes()).map_err(|_| "The browser closed.")?;
                                        r.filter.choosers.remove(&session);
                                        Ok(None)
                                    } else {
                                        let paths: Vec<String> = serde_json::from_value(params["paths"].clone()).map_err(|_| "Invalid upload paths.")?;
                                        let paths: Vec<PathBuf> = paths.iter().map(|p| access.resolve(p, false)).collect::<Result<_,_>>()?;
                                        let sources = agent_upload::read_sources(&paths, check)?;
                                        Ok(Some(serde_json::to_value(sources).map_err(|e| e.to_string())?))
                                    }
                                });
                                self.0.log_act(actor, result.as_ref().err());
                                match result {
                                    Ok(None) => continue,
                                    Ok(Some(v)) => Ok(v),
                                    Err(e) => Err(e),
                                }
                            }
                            Own::ChooseFiles {
                                session,
                                node,
                                files,
                            } => {
                                if stale {
                                    Err(STOPPED_SINCE_START.to_string())
                                } else {
                                    match r.uploads.stage(&files) {
                                        Ok(paths) => {
                                            r.filter.choosers.remove(&session);
                                            let msg = json!({"id":id,"sessionId":session,"method":"DOM.setFileInputFiles","params":{"backendNodeId":node,"files":paths}});
                                            if r.tx.send(msg.to_string().into_bytes()).is_ok() {
                                                continue;
                                            }
                                            Err("the browser closed".into())
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                            }
                            Own::UploadFiles {
                                session,
                                node,
                                owner,
                                action,
                            } => {
                                if stale {
                                    Err(STOPPED_SINCE_START.to_string())
                                } else {
                                    use tessera_proto::BrowserUploadAction::*;
                                    match action {
                                        Start { files } => {
                                            let weak = Arc::downgrade(&self.0);
                                            tokio::spawn(async move {
                                                tokio::time::sleep(Duration::from_secs(3601)).await;
                                                if let Some(inner) = weak.upgrade()
                                                    && let Some(r) = inner
                                                        .run
                                                        .lock()
                                                        .as_mut()
                                                        .filter(|r| r.run == run)
                                                {
                                                    r.uploads.expire();
                                                }
                                            });
                                            r.uploads
                                                .start(&owner, &files)
                                                .map(|token| json!({"upload":token,"received":0}))
                                        }
                                        Chunk {
                                            upload,
                                            index,
                                            data,
                                        } => r
                                            .uploads
                                            .chunk(&owner, &upload, index, &data)
                                            .map(|n| json!({"received":n})),
                                        Cancel { upload } => r
                                            .uploads
                                            .cancel(&owner, &upload)
                                            .map(|_| json!({"received":0})),
                                        Finish { upload } => {
                                            if !r
                                                .filter
                                                .choosers
                                                .get(&session)
                                                .is_some_and(|(n, _)| *n == node)
                                            {
                                                let _ = r.uploads.cancel(&owner, &upload);
                                                Err("that file chooser closed".into())
                                            } else {
                                                match r.uploads.finish(&owner, &upload) {
                                                    Ok(paths) => {
                                                        let msg = json!({"id":id,"sessionId":session,"method":"DOM.setFileInputFiles","params":{"backendNodeId":node,"files":paths}});
                                                        if r.tx
                                                            .send(msg.to_string().into_bytes())
                                                            .is_ok()
                                                        {
                                                            r.upload_finishes.insert(
                                                                id,
                                                                (
                                                                    owner.clone(),
                                                                    upload.clone(),
                                                                    session.clone(),
                                                                    node,
                                                                ),
                                                            );
                                                            continue;
                                                        }
                                                        let _ = r.uploads.cancel(&owner, &upload);
                                                        Err("the browser closed".into())
                                                    }
                                                    Err(e) => {
                                                        let _ = r.uploads.cancel(&owner, &upload);
                                                        Err(e)
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Own::DiscardDownload(guid) => {
                                if let Some(p) = r.saved.remove(&guid)
                                    && p.starts_with(&r.downloads)
                                {
                                    let _ = std::fs::remove_file(&p);
                                }
                                Ok(json!({}))
                            }
                        };
                        answers.push(match answer {
                            Ok(v) => json!({"id": id, "result": v}).to_string(),
                            Err(e) => json!({"id": id, "error": {"code": -32000, "message": e}})
                                .to_string(),
                        });
                    }
                    Err(refused) => {
                        if let Some(a) = refusal(&refused) {
                            answers.push(a);
                        }
                        refusals.push(refused);
                    }
                }
            }
        }
        for r in refusals {
            self.0.log_refusal(&r);
        }
        if !answers.is_empty() {
            let _ = self.0.send_up(NodeBrowserUp::Cdp {
                run,
                messages: answers,
            });
        }
    }
}

impl Inner {
    fn take_stopping(&self, run: u64) -> Option<Run> {
        let mut cur = self.run.lock();
        if !cur.as_ref().is_some_and(|r| r.run == run) {
            return None;
        }
        let r = cur.take()?;
        self.stopping.lock().insert(run, r.stop.clone());
        Some(r)
    }

    fn begin_screen(&self, stops: u32) -> Result<ScreenLaunch, String> {
        let mut launches = self.screen_launch.lock();
        let live = super::StopLatch::load(&self.paths);
        if live.stopped || live.stops != stops {
            return Err(STOPPED_SINCE_START.into());
        }
        if let Some(p) = launches.pending.take() {
            p.cancel.cancel();
        }
        launches.next = launches.next.wrapping_add(1);
        let launch = ScreenLaunch {
            id: launches.next,
            stops,
            cancel: CancellationToken::new(),
        };
        launches.pending = Some(launch.clone());
        Ok(launch)
    }

    fn cancel_screen_launch(&self) {
        if let Some(p) = self.screen_launch.lock().pending.take() {
            p.cancel.cancel();
        }
    }

    #[cfg(unix)]
    fn screen_admitted(
        &self,
        launch: &ScreenLaunch,
        launches: &ScreenLaunches,
    ) -> Result<(), String> {
        let live = super::StopLatch::load(&self.paths);
        if launch.cancel.is_cancelled()
            || live.stopped
            || live.stops != launch.stops
            || !launches.pending.as_ref().is_some_and(|p| p.id == launch.id)
        {
            return Err(STOPPED_SINCE_START.into());
        }
        Ok(())
    }

    #[cfg(unix)]
    fn spawn_screen(
        &self,
        launch: &ScreenLaunch,
        cmd: &mut tokio::process::Command,
        lease: crate::browser::process::ExitLease,
    ) -> Result<ScreenChild, String> {
        // Same gate as Stop: no spawn with an already-revoked admission.
        let launches = self.screen_launch.lock();
        self.screen_admitted(launch, &launches)?;
        let child = cmd
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("can't start the browser: {e}"))?;
        let pid = child.id();
        let child = ScreenChild {
            child: Some(child),
            pgid: pid,
            lease: Some(lease),
        };
        // Also catch a file/guard Stop before the node loop observes it.
        self.screen_admitted(launch, &launches)?;
        Ok(child)
    }

    #[cfg(unix)]
    async fn screen_child_ready(
        &self,
        launch: &ScreenLaunch,
        child: &mut ScreenChild,
    ) -> Result<(), String> {
        let log = crate::browser::process::keep_log(child.child.as_mut().expect("screen child"));
        let ready = launch
            .wait(crate::browser::process::wait_exit(
                child.child.as_mut().expect("screen child"),
                Duration::from_millis(2500),
            ))
            .await;
        match ready {
            Err(e) => {
                child.cleanup().await;
                Err(e)
            }
            Ok(true) => {
                let status = child.cleanup().await;
                let why = crate::browser::process::why_failed(&log).unwrap_or_else(|| {
                    format!(
                        "it ended at once ({})",
                        status.map(|s| s.to_string()).unwrap_or_default()
                    )
                });
                Err(format!(
                    "the window couldn't open: {}",
                    crate::util::one_line(&why, 300)
                ))
            }
            Ok(false) => Ok(()),
        }
    }
    /// Open the window on the screen: the browser for web views stops first
    /// (asked to quit, so it saves its sign-ins), then an ordinary browser
    /// starts on the same profile with this computer's display.
    async fn open_screen(self: Arc<Self>, url: &str, launch: &ScreenLaunch) -> Result<(), String> {
        #[cfg(not(unix))]
        {
            let _ = (url, launch.stops);
            Err("showing pages on this computer's screen isn't supported on its system yet".into())
        }
        #[cfg(unix)]
        {
            self.clone()
                .open_screen_with(url, launch, async {
                    tokio::task::spawn_blocking(super::desktop::discover)
                        .await
                        .ok()
                        .flatten()
                })
                .await
        }
    }

    #[cfg(unix)]
    async fn open_screen_with(
        self: Arc<Self>,
        url: &str,
        launch: &ScreenLaunch,
        discover: impl std::future::Future<Output = Option<super::desktop::Session>>,
    ) -> Result<(), String> {
        let session=launch.wait(discover).await?
                .ok_or("nobody is signed in to this computer's desktop, so there's no screen to show the page on")?;
        launch.wait(self.close_screen()).await?;
        // A removed old run must still stop if this launch is cancelled.
        let run = self.run.lock().take();
        if let Some(r) = run {
            struct Quit(CancellationToken);
            impl Drop for Quit {
                fn drop(&mut self) {
                    self.0.cancel();
                }
            }
            let _quit = Quit(r.stop.clone());
            r.asked.store(true, Ordering::SeqCst);
            launch
                .wait(async {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        r.own.call(&r.tx, "Browser.close", json!({})),
                    )
                    .await;
                    wait_gone(r.pid, Duration::from_secs(8)).await;
                    r.stop.cancel();
                    wait_gone(r.pid, Duration::from_secs(3)).await;
                })
                .await?;
            let _ = self.send_up(NodeBrowserUp::Stopped {
                run: r.run,
                reason: Some("the page is shown on that computer's screen".into()),
                crashed: false,
            });
        }
        let profile_guard = launch
            .wait(crate::browser::process::bounded_admission(
                self.profile_gate.clone().lock_owned(),
            ))
            .await??;
        self.screen_admitted(launch, &self.screen_launch.lock())?;
        let bin = crate::browser::process::find(None).ok_or(NO_BROWSER)?;
        let base = dir(&self.paths);
        let (profile, home, tmp) = (base.join("profile"), base.join("home"), base.join("tmp"));
        for d in [&base, &profile, &home, &tmp] {
            private_dir(d).map_err(|e| format!("can't create {}: {e}", d.display()))?;
        }
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(screen_args(&profile, url));
        cmd.env_clear();
        for (k, v) in browser_env(&home, &tmp)
            .into_iter()
            .chain(screen_env(&session))
        {
            cmd.env(k, v);
        }
        // The display's credentials (X11) are the one thing outside its
        // folder it may read.
        let extra: Vec<PathBuf> = session
            .vars
            .iter()
            .filter(|(k, _)| k == "XAUTHORITY")
            .map(|(_, v)| PathBuf::from(v))
            .collect();
        let mut confine = confine::plan_with(&bin, &base, &extra);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                if let Some(c) = confine.as_mut() {
                    c.apply()?;
                }
                Ok(())
            });
        }
        let lease =
            crate::browser::process::ExitLease::new(Some(profile_guard), CancellationToken::new());
        let mut child = self.spawn_screen(launch, &mut cmd, lease)?;
        self.screen_child_ready(launch, &mut child).await?;
        let pid = child.pgid;
        let close = launch.cancel.clone();
        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (gone_tx, gone) = tokio::sync::watch::channel(false);
        {
            let launches = self.screen_launch.lock();
            self.screen_admitted(launch, &launches)?;
            *self.screen.lock() = Some(Screen {
                pid,
                close: close.clone(),
                asked: asked.clone(),
                gone,
            });
        }
        self.log_line(
            NodeLogEvent::Done,
            format!(
                "web views: showed {} on this computer's screen ({})",
                crate::util::one_line(&crate::tools::scrub_patterns(url), 200),
                session.label
            ),
        );
        let me = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = crate::browser::process::wait_exit(child.child.as_mut().expect("screen child"), Duration::from_secs(86400 * 365)) => {}
                _ = close.cancelled() => {
                    // Asked to quit, it saves what changed (cookies, sign-ins).
                    if let Some(pid) = pid {
                        // SAFETY: a signal to our own child.
                        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    }
                    let _ = crate::browser::process::wait_exit(child.child.as_mut().expect("screen child"), Duration::from_secs(8)).await;
                }
            }
            child.cleanup().await;
            let _ = gone_tx.send(true);
            {
                let mut cur = me.screen.lock();
                if cur.as_ref().is_some_and(|s| s.pid == pid) {
                    *cur = None;
                }
            }
            me.log_line(
                NodeLogEvent::Done,
                "web views: the window on this computer's screen closed".into(),
            );
            // Closed there by the user: the server's tiles can carry on.
            if !asked.load(Ordering::SeqCst) {
                let _ = me.up.lock().tx.send(NodeBrowserUp::Screen {
                    shown: false,
                    error: None,
                });
            }
        });
        Ok(())
    }

    /// Close the window on the screen, if one is open, and wait until it
    /// has (it saves what changed first).
    async fn close_screen(&self) {
        let s = self.screen.lock().take();
        if let Some(s) = s {
            s.asked.store(true, Ordering::SeqCst);
            s.close.cancel();
            let mut gone = s.gone;
            let _ = tokio::time::timeout(Duration::from_secs(12), gone.wait_for(|g| *g)).await;
        }
    }

    fn log_line(&self, event: NodeLogEvent, detail: String) {
        let mut e = entry(event, detail);
        e.action = Some("web".into());
        self.log.add(&e);
    }

    fn log_act(&self, act: &Value, refused: Option<&String>) {
        let bot: Option<NodeBot> = serde_json::from_value(act["bot"].clone()).ok();
        let action = crate::util::one_line(act["action"].as_str().unwrap_or("act"), 40);
        let target = act["target"].as_str().unwrap_or_default();
        let mut e = entry(
            if refused.is_some() {
                NodeLogEvent::Refused
            } else {
                NodeLogEvent::Done
            },
            match refused {
                Some(why) => format!("web view {action}: {why}"),
                None => format!("web view {action}"),
            },
        );
        e.session_id = act["session"]
            .as_str()
            .map(|s| crate::util::one_line(s, 80));
        e.chat = act["chat"].as_str().map(|s| crate::util::one_line(s, 120));
        e.action = Some("web".into());
        e.target = Some(crate::util::truncate(
            &crate::tools::scrub_patterns(&crate::util::visible_controls(target)),
            400,
        ))
        .filter(|t| !t.is_empty());
        e.bot = bot.map(|b| crate::util::one_line(&b.name, 80));
        self.log.add(&e);
    }

    fn log_refusal(&self, r: &Refused) {
        {
            let mut l = self.logged.lock();
            if l.0.elapsed() > Duration::from_secs(60) {
                *l = (Instant::now(), 0);
            }
            l.1 += 1;
            if l.1 > 20 {
                return;
            }
        }
        let what = if r.method.is_empty() {
            "a command".to_string()
        } else {
            format!("`{}`", crate::util::one_line(&r.method, 80))
        };
        self.log_line(
            NodeLogEvent::Refused,
            format!("web views: refused {what} from the server: {}", r.reason),
        );
    }

    /// On failure remember the run even if its process concurrently exits.
    fn send_up(&self, message: NodeBrowserUp) -> Result<(), ()> {
        let run = match &message {
            NodeBrowserUp::Cdp { run, .. }
            | NodeBrowserUp::Started { run, .. }
            | NodeBrowserUp::Stopped { run, .. }
            | NodeBrowserUp::Failed { run, .. } => Some(*run),
            _ => None,
        };
        let mut up = self.up.lock();
        let result = up.tx.send(message);
        if result.is_err() && !up.connection.is_cancelled() && up.failed.is_none() {
            up.failed = run;
        }
        result
    }

    /// Critical recovery acknowledgements retry until the connection ends.
    /// A lost acknowledgement is safe: the server ignores already-seen seqs.
    async fn acknowledged(
        link: &dyn Link,
        connection: &CancellationToken,
        request: Request,
    ) -> bool {
        loop {
            tokio::select! {
                biased;
                _ = connection.cancelled() => return false,
                result = tokio::time::timeout(Duration::from_secs(3), link.call(request.clone())) => {
                    if matches!(result, Ok(Ok(_))) { return true; }
                }
            }
            tokio::select! {
                _ = connection.cancelled() => return false,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }

    /// Send news in order. Overflow is supervised, not a permanently poisoned
    /// connection: terminate the run, reliably report failure, then rearm.
    fn forward_with(self: Arc<Self>, link: Arc<dyn Link>) {
        let (mut rx, mut link_stop, connection) = {
            let mut up = self.up.lock();
            if up.forwarding {
                return;
            }
            up.forwarding = true;
            let Some(rx) = up.rx.take() else {
                return;
            };
            (rx, up.stop.clone(), up.connection.clone())
        };
        tokio::spawn(async move {
            let mut flight = futures::stream::FuturesOrdered::<
                futures::future::BoxFuture<'static, Result<Value, tessera_proto::ApiError>>,
            >::new();
            let mut pending = std::collections::VecDeque::new();
            loop {
                tokio::select! {
                    biased;
                    _ = connection.cancelled() => break,
                    _ = link_stop.cancelled() => {
                        // Drop leases and cancelled RPCs before any slow retries.
                        flight.clear();
                        rx.close();
                        while rx.try_recv().is_ok() {}
                        let failed = self.up.lock().failed.take();
                        let stopped = self.run.lock().take().map(|r| {
                            r.asked.store(true, Ordering::SeqCst);
                            r.stop.cancel();
                            r.run
                        });
                        // Cancelled RPCs may have consumed sequence numbers.
                        // Fill each abandoned gap, in order; never reset seq on
                        // a live connection or pretend an unacknowledged send worked.
                        for seq in pending.drain(..) {
                            if !Self::acknowledged(link.as_ref(), &connection,
                                Request::NodeBrowser { seq, msgs: vec![] }).await { return; }
                        }
                        let error = "web view stopped: it produced more than the link could carry";
                        self.log_line(NodeLogEvent::Failed, error.into());
                        let mut report = stopped.or(failed);
                        loop {
                            if let Some(run) = report {
                                let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
                                if !Self::acknowledged(link.as_ref(), &connection,
                                    Request::NodeBrowser { seq, msgs: vec![
                                        // Stopped records why even for an already
                                        // started view; Failed also answers startup.
                                        NodeBrowserUp::Stopped { run, reason: Some(error.into()), crashed: true },
                                        NodeBrowserUp::Failed { run, error: error.into() },
                                    ] }).await { return; }
                            }
                            // No wait under node/Stop locks. Atomically drain the
                            // latest refused Start or make future Starts healthy.
                            let mut up = self.up.lock();
                            if connection.is_cancelled() { return; }
                            report = up.rejected.take();
                            if report.is_none() {
                                link_stop = CancellationToken::new();
                                let (tx, next) = queue::channel(64 << 20, link_stop.clone());
                                up.tx = tx;
                                up.stop = link_stop.clone();
                                up.failed = None;
                                rx = next;
                                break;
                            }
                        }
                    }
                    Some(result) = flight.next(), if !flight.is_empty() => {
                        if result.is_ok() { pending.pop_front(); }
                        else { link_stop.cancel(); }
                    }
                    m = rx.recv(), if flight.len() < IN_FLIGHT => {
                        let Some(first) = m else { break };
                        let mut leases = vec![first.lease];
                        let mut msgs = vec![first.value];
                        let mut bytes = queue::Size::bytes(&msgs[0]);
                        while bytes < BATCH_BYTES {
                            match rx.try_recv() {
                                Ok(item) => {
                                    leases.push(item.lease);
                                    let message = item.value;
                                    bytes += queue::Size::bytes(&message);
                                    match message {
                                        NodeBrowserUp::Cdp { run, messages } => match msgs.last_mut() {
                                            Some(NodeBrowserUp::Cdp { run: r, messages: m }) if *r == run => m.extend(messages),
                                            _ => msgs.push(NodeBrowserUp::Cdp { run, messages }),
                                        },
                                        other => msgs.push(other),
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
                        pending.push_back(seq);
                        let call = link.call(Request::NodeBrowser { seq, msgs });
                        flight.push_back(Box::pin(async move {
                            let _leases = leases;
                            call.await
                        }));
                    }
                    else => break,
                }
            }
        });
    }

    /// Start the browser, connect to it and set it up. Returns its product
    /// and what its drawing speed depends on.
    #[allow(clippy::too_many_arguments)]
    async fn launch(
        self: Arc<Self>,
        run: u64,
        bin: &Path,
        base: &Path,
        scale: Option<f64>,
        tx: queue::Sender<Vec<u8>>,
        rx: queue::Receiver<Vec<u8>>,
        own: Arc<OwnCalls>,
        stop: CancellationToken,
        asked: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(String, String), String> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (run, bin, base, scale, tx, rx, own, stop, asked);
            Err("web views on this computer's system aren't supported yet".into())
        }
        #[cfg(any(unix, windows))]
        {
            let profile_guard = tokio::select! {
                biased;
                _ = stop.cancelled() => return Err("the browser start was cancelled".into()),
                gate = crate::browser::process::bounded_admission(self.profile_gate.clone().lock_owned()) => match gate {
                    Ok(guard) => guard,
                    Err(error) => { asked.store(true, Ordering::SeqCst); return Err(error); }
                },
            };
            if stop.is_cancelled() {
                return Err("the browser start was cancelled".into());
            }
            let (profile, downloads, home, tmp) = (
                base.join("profile"),
                base.join("downloads"),
                base.join("home"),
                base.join("tmp"),
            );
            for d in [base, &profile, &downloads, &home, &tmp] {
                private_dir(d).map_err(|e| format!("can't create {}: {e}", d.display()))?;
            }
            if let Some(r) = self.run.lock().as_mut().filter(|r| r.run == run) {
                r.uploads.reset()?;
            }
            #[cfg(windows)]
            let private_folders = [base, &profile, &downloads, &home, &tmp]
                .into_iter()
                .map(crate::guard::windows::private_browser_dir)
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|e| format!("browser folders could not be protected: {e}"))?;
            clean_partial(&downloads);
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args(&profile, scale));
            // Nothing of the desktop session: no display, clipboard, keyring
            // or session bus; its own home and temporary folder.
            cmd.env_clear();
            for (k, v) in browser_env(&home, &tmp) {
                cmd.env(k, v);
            }
            #[cfg(unix)]
            let confine = confine::plan(bin, base);
            #[cfg(unix)]
            let landlock = confine.is_some();
            #[cfg(windows)]
            let landlock = false;
            #[cfg(unix)]
            let hook: Option<Box<dyn FnMut() -> std::io::Result<()> + Send + Sync>> =
                confine.map(|c| Box::new(move || c.apply()) as Box<_>);
            #[cfg(unix)]
            let (mut child, (reader, writer)) =
                crate::browser::process::spawn_piped(&mut cmd, hook)?;
            #[cfg(windows)]
            let (mut child, job) = super::browser_windows::spawn(
                bin,
                base,
                scale,
                &browser_env(&home, &tmp),
                memory_cap(),
            )?;
            #[cfg(windows)]
            let (reader, writer) = (
                child.stdout.take().ok_or("no browser output pipe")?,
                child.stdin.take().ok_or("no browser input pipe")?,
            );
            let log = crate::browser::process::keep_log(&mut child);
            let pid = child.id();
            if let Some(r) = self.run.lock().as_mut().filter(|r| r.run == run) {
                r.pid = pid;
                #[cfg(windows)]
                {
                    r.job = Some(job.clone());
                }
            }
            tokio::spawn(write_pipe(writer, rx, stop.clone()));
            tokio::spawn(self.clone().read_pipe(
                run,
                reader,
                own.clone(),
                stop.clone(),
                asked.clone(),
            ));
            // Its exit: stopped on purpose, or unexpectedly (the server is told).
            let me = self.clone();
            let stop2 = stop.clone();
            tokio::spawn(async move {
                let lease = crate::browser::process::ExitLease::new(
                    Some(profile_guard),
                    CancellationToken::new(),
                );
                tokio::select! {
                    _ = crate::browser::process::wait_exit(&mut child, Duration::from_secs(86400 * 365)) => {}
                    _ = stop2.cancelled() => {}
                }
                // Kill before reaping the leader, which still pins this group.
                #[cfg(unix)]
                kill_group(pid);
                #[cfg(windows)]
                job.end();
                let _ = child.start_kill();
                #[cfg(unix)]
                let exit = crate::browser::process::confirmed_exit(child);
                #[cfg(windows)]
                let exit = async move {
                    let status = crate::browser::process::confirmed_exit(child).await;
                    job.wait_empty().await;
                    // Keep folder protections with the reserved profile too.
                    drop(private_folders);
                    status
                };
                let _ =
                    crate::browser::process::hold_until_exit(exit, lease, Duration::from_secs(2))
                        .await;
                stop2.cancel();
                if !asked.load(Ordering::SeqCst) {
                    let current = {
                        let mut cur = me.run.lock();
                        let current = cur.as_ref().is_some_and(|r| r.run == run);
                        if current {
                            *cur = None;
                        }
                        current
                    };
                    if current {
                        me.log_line(
                            NodeLogEvent::Failed,
                            "web views: the browser stopped unexpectedly".into(),
                        );
                        let _ = me.up.lock().tx.send(NodeBrowserUp::Stopped {
                            run,
                            reason: Some("it stopped unexpectedly".into()),
                            crashed: true,
                        });
                    }
                }
            });
            let version = match own.call(&tx, "Browser.getVersion", json!({})).await {
                Ok(v) => v,
                Err(_e) => {
                    let why = crate::browser::process::why_failed(&log)
                        .unwrap_or_else(|| "Chrome didn't answer the startup probe".into());
                    stop.cancel();
                    return Err(format!(
                        "the browser didn't start{}: {}",
                        if landlock { " in its file sandbox" } else { "" },
                        crate::util::one_line(&why, 300)
                    ));
                }
            };
            let product = version["product"]
                .as_str()
                .unwrap_or("Chromium")
                .to_string();
            // Downloads go only to its own folder; only pages are attached.
            for (method, params) in [
                (
                    "Browser.setDownloadBehavior",
                    json!({"behavior": "allowAndName", "downloadPath": downloads.display().to_string(), "eventsEnabled": true}),
                ),
                ("Target.setDiscoverTargets", json!({"discover": true})),
                (
                    "Target.setAutoAttach",
                    json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true, "filter": [{"type": "page"}, {"exclude": true}]}),
                ),
            ] {
                if let Err(e) = own.call(&tx, method, params).await {
                    stop.cancel();
                    return Err(format!("couldn't set up the browser: {e}"));
                }
            }
            if !landlock {
                tracing::info!(
                    "web views: no Landlock here; the browser runs with its own sandbox and the command allowlist"
                );
            }
            Ok((product.clone(), speed_key(&product)))
        }
    }

    /// What the browser sends: learnt from, then passed to the server.
    #[cfg(any(unix, windows))]
    async fn read_pipe(
        self: Arc<Self>,
        run: u64,
        reader: PipeReader,
        own: Arc<OwnCalls>,
        stop: CancellationToken,
        asked: Arc<std::sync::atomic::AtomicBool>,
    ) {
        use tokio::io::AsyncReadExt;
        let mut reader = reader;
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
        let mut chunk = vec![0u8; 1 << 16];
        let mut scanned = 0;
        'read: loop {
            let n = tokio::select! {
                r = reader.read(&mut chunk) => match r {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                },
                _ = stop.cancelled() => break,
            };
            buf.extend_from_slice(&chunk[..n]);
            let mut batch = Vec::new();
            loop {
                let Some(end) = buf[scanned..].iter().position(|b| *b == 0) else {
                    scanned = buf.len();
                    if buf.len() > MAX_MESSAGE {
                        break 'read;
                    }
                    break;
                };
                let end = scanned + end;
                if end > MAX_MESSAGE {
                    break 'read;
                }
                if let Some(out) = self.on_message(run, &buf[..end], &own) {
                    batch.push(out);
                }
                buf.drain(..=end);
                scanned = 0;
            }
            if !batch.is_empty()
                && self
                    .send_up(NodeBrowserUp::Cdp {
                        run,
                        messages: batch,
                    })
                    .is_err()
            {
                break;
            }
        }
        // During Browser.close the pipe may finish before cookie flushing
        // and process exit. Let child.wait finish instead of killing it here.
        if !asked.load(Ordering::SeqCst) {
            stop.cancel();
        }
    }

    /// One message from the browser: what the server gets of it, if
    /// anything.
    fn on_message(&self, run: u64, raw: &[u8], own: &OwnCalls) -> Option<String> {
        let mut v: Value = serde_json::from_slice(raw).ok()?;
        if own.answer(&v) {
            return None;
        }
        let mut cur = self.run.lock();
        let r = cur.as_mut().filter(|r| r.run == run)?;
        if let Some(id) = v["id"].as_u64()
            && let Some((owner, token, session, node)) = r.upload_finishes.remove(&id)
        {
            if v.get("error").is_some() {
                let _ = r.uploads.cancel(&owner, &token);
            } else if let Err(error) = r.uploads.commit(&owner, &token) {
                v = json!({"id":id,"sessionId":session,"error":{"code":-32000,"message":error}});
            } else if r
                .filter
                .choosers
                .get(&session)
                .is_some_and(|(n, _)| *n == node)
            {
                r.filter.choosers.remove(&session);
            }
        }
        if let Some(target) = r.filter.learn(&v) {
            // Too many pages: this one closes at once.
            own.fire(&r.tx, "Target.closeTarget", json!({"targetId": target}));
            return None;
        }
        match v["method"].as_str().unwrap_or_default() {
            "Browser.downloadWillBegin" => {
                let guid = v["params"]["guid"].as_str().unwrap_or_default().to_string();
                let name = crate::browser::safe_name(
                    v["params"]["suggestedFilename"]
                        .as_str()
                        .unwrap_or("download"),
                );
                r.names.insert(guid, name);
            }
            "Browser.downloadProgress" => {
                let guid = v["params"]["guid"].as_str().unwrap_or_default().to_string();
                let from = r.downloads.join(crate::browser::safe_name(&guid));
                match v["params"]["state"].as_str().unwrap_or_default() {
                    "inProgress" => {
                        let got = v["params"]["receivedBytes"].as_u64().unwrap_or(0);
                        r.receiving.insert(guid.clone(), got);
                        let total = r.kept.saturating_add(r.receiving.values().sum());
                        if got > MAX_DOWNLOAD || total > MAX_DOWNLOADS_TOTAL {
                            own.fire(&r.tx, "Browser.cancelDownload", json!({"guid": guid}));
                        }
                    }
                    "completed" => {
                        r.kept = r
                            .kept
                            .saturating_add(r.receiving.remove(&guid).unwrap_or(0));
                        let name = r.names.remove(&guid).unwrap_or_else(|| "download".into());
                        let to = unique_path(&r.downloads, &name);
                        if std::fs::rename(&from, &to).is_ok() {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = std::fs::set_permissions(
                                    &to,
                                    std::fs::Permissions::from_mode(0o600),
                                );
                            }
                            v["params"]["tesseraPath"] = json!(to.display().to_string());
                            r.saved.insert(guid, to);
                        }
                    }
                    "canceled" => {
                        r.names.remove(&guid);
                        r.receiving.remove(&guid);
                        let _ = std::fs::remove_file(&from);
                    }
                    _ => {}
                }
                return Some(v.to_string());
            }
            _ => {}
        }
        Some(String::from_utf8_lossy(raw).into_owned())
    }

    /// Stop the browser when unused for long, or when it uses too much
    /// memory.
    async fn watch(
        self: Arc<Self>,
        run: u64,
        stop: CancellationToken,
        last_use: Arc<Mutex<Instant>>,
    ) {
        let cap = memory_cap();
        // One sample over the cap first warns and gives the server a watch
        // interval to shed a page, even after a sudden allocation jump.
        let mut over_since: Option<Instant> = None;
        let mut asked_at: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
            let pid = {
                let cur = self.run.lock();
                match cur.as_ref() {
                    Some(r) if r.run == run => r.pid,
                    _ => return,
                }
            };
            #[cfg(windows)]
            let memory = self
                .run
                .lock()
                .as_ref()
                .filter(|r| r.run == run)
                .and_then(|r| r.job.as_ref())
                .and_then(|j| j.memory());
            #[cfg(not(windows))]
            let memory = memory_of(pid);
            #[cfg(windows)]
            let _ = pid;
            let now = Instant::now();
            let first_over = memory.is_some_and(|m| m > cap) && over_since.is_none();
            let stop_for_memory = memory_stop_due(memory, cap, &mut over_since, now);
            // Windows warns below its enforced peak-commit ceiling. Unix
            // also warns early when this computer runs short of memory.
            let warn = memory.is_some_and(|m| {
                memory_warn_due(m, cap, cfg!(windows))
                    || (!cfg!(windows) && should_shed(m, cap, crate::budget::mem_available()))
            });
            if warn
                && (first_over
                    || asked_at.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(15)))
            {
                asked_at = Some(now);
                let m = memory.unwrap_or(0);
                self.log_line(NodeLogEvent::Done, format!(
                    "web views: the browser uses {:.1} of its {:.1} GB; asked to close a page nobody needs",
                    m as f64 / (1u64 << 30) as f64, cap as f64 / (1u64 << 30) as f64
                ));
                let event = json!({
                    "method": crate::browser::MEMORY_HIGH,
                    "params": {"used": m, "cap": cap, "over": first_over,
                        "accounting": if cfg!(windows) { "peak_commit" } else { "pss" }},
                });
                let _ = self.send_up(NodeBrowserUp::Cdp {
                    run,
                    messages: vec![event.to_string()],
                });
            }
            let why = if last_use.lock().elapsed() > IDLE {
                Some(format!(
                    "it wasn't used for {} minutes",
                    IDLE.as_secs() / 60
                ))
            } else {
                memory.filter(|_| stop_for_memory).map(|m| {
                    format!(
                        "it used {:.1} GB of memory, more than the {:.1} GB allowed here",
                        m as f64 / (1u64 << 30) as f64,
                        cap as f64 / (1u64 << 30) as f64
                    )
                })
            };
            if let Some(why) = why {
                let Some(r) = self.take_stopping(run) else {
                    return;
                };
                r.save_and_stop().await;
                self.stopping.lock().remove(&run);
                self.log_line(
                    NodeLogEvent::Done,
                    format!("web views: the browser stopped ({why})"),
                );
                let _ = self.send_up(NodeBrowserUp::Stopped {
                    run,
                    reason: Some(why),
                    crashed: false,
                });
                return;
            }
        }
    }
}

/// Windows reports peak committed bytes for the whole owned Job (not
/// PSS/current working set). Warn at 80% before its hard commit ceiling;
/// the OS may deny a sudden allocation before any periodic sample can run.
fn memory_warn_due(used: u64, cap: u64, windows: bool) -> bool {
    used >= if windows {
        cap.saturating_mul(4) / 5
    } else {
        cap
    }
}

/// Require two over-cap samples one watch interval apart; the first gets
/// a warning. A recovered/unknown reading cancels the pending stop.
fn memory_stop_due(used: Option<u64>, cap: u64, since: &mut Option<Instant>, now: Instant) -> bool {
    if used.is_some_and(|m| m > cap) {
        now.duration_since(*since.get_or_insert(now)) >= Duration::from_secs(5)
    } else {
        *since = None;
        false
    }
}

/// Should the server close a page to save memory: the browser uses
/// three quarters of its `cap`, or this computer has less than an eighth of
/// its memory (at least 1 GB) left and the browser uses half a GB or more.
#[cfg_attr(windows, allow(dead_code))]
fn should_shed(used: u64, cap: u64, available: Option<u64>) -> bool {
    used >= cap / 4 * 3
        || (used >= 512 << 20 && available.is_some_and(|a| a < crate::budget::low_memory_mark()))
}

/// Kill a browser's whole process group.
#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: plain syscall on our own child's process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Memory a process group uses, in bytes (Linux): the sum of each
/// process's proportional share (PSS), so pages its processes share (the
/// browser's code, shared memory between them) count once in all, as the
/// system counts them. A process whose share can't be read counts with its
/// whole resident size. (Summing resident sizes counted Chrome's shared
/// pages once per process: four times its real use on a test.)
#[cfg(not(windows))]
fn memory_of(pgid: Option<u32>) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let pgid = pgid?;
        let page = 4096u64;
        let mut total = 0u64;
        for e in std::fs::read_dir("/proc").ok()?.flatten() {
            let name = e.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // Fields after the command (which may hold spaces): state ppid pgrp …
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.get(2).and_then(|g| g.parse::<u32>().ok()) != Some(pgid) {
                continue;
            }
            if let Some(pss) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                .ok()
                .and_then(|s| pss_of(&s))
            {
                total += pss;
            } else if let Ok(statm) = std::fs::read_to_string(format!("/proc/{pid}/statm"))
                && let Some(rss) = statm
                    .split_whitespace()
                    .nth(1)
                    .and_then(|r| r.parse::<u64>().ok())
            {
                total += rss * page;
            }
        }
        Some(total)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pgid;
        None
    }
}

/// The proportional share (`Pss:`) in a process's `smaps_rollup`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pss_of(smaps_rollup: &str) -> Option<u64> {
    let line = smaps_rollup.lines().find(|l| l.starts_with("Pss:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The most memory the browser may use: a quarter of this computer's (or
/// its container's), between 1 and 8 GB.
fn memory_cap() -> u64 {
    cap_for(crate::budget::machine_ram())
}

/// [`memory_cap`] for a computer with `ram` bytes (unknown: as for 8 GB).
fn cap_for(ram: Option<u64>) -> u64 {
    (ram.unwrap_or(8 << 30) / 4).clamp(1 << 30, 8 << 30)
}

/// What a machine's drawing speed depends on: the browser and the CPU.
fn speed_key(product: &str) -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    format!("{product}|{cpu}|{cores}")
}

/// Wait (up to `limit`) until process `pid` is gone.
#[cfg(unix)]
async fn wait_gone(pid: Option<u32>, limit: Duration) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let until = Instant::now() + limit;
        // SAFETY: signal 0 only asks whether the process exists.
        while unsafe { libc::kill(pid as i32, 0) } == 0 && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, limit);
}

/// The window on this computer's screen: an ordinary browser on the web
/// views' profile (the same sign-ins). No debugging pipe and no automation
/// flags (nobody drives it); security keys still off, as in web views; no
/// extensions and nothing in the background. `url` last, in a new window.
pub fn screen_args(profile: &Path, url: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "--no-first-run",
        "--no-default-browser-check",
        "--password-store=basic",
        "--use-mock-keychain",
        "--disable-sync",
        "--disable-extensions",
        "--disable-default-apps",
        "--disable-background-networking",
        "--disable-breakpad",
        "--no-pings",
        "--disable-blink-features=WebAuth",
        "--ozone-platform-hint=auto",
        "--new-window",
    ]
    .map(String::from)
    .into();
    a.insert(0, format!("--user-data-dir={}", profile.display()));
    a.push(url.to_string());
    a
}

/// What the window takes of the desktop session: the display, nothing
/// else (no session bus, so no keyring or wallet; no window manager's
/// control socket).
#[cfg(any(unix, test))]
fn screen_env(session: &super::desktop::Session) -> Vec<(String, String)> {
    const DISPLAY: &[&str] = &[
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
    ];
    session
        .vars
        .iter()
        .filter(|(k, _)| DISPLAY.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// The flags: the server's (pipe only, never a port; Chromium's sandbox on)
/// plus no extensions and nothing running in the background.
pub fn args(profile: &Path, scale: Option<f64>) -> Vec<String> {
    let mut a = crate::browser::process::args(profile, true, scale);
    let blank = a.pop();
    a.extend(
        [
            "--disable-extensions",
            "--disable-default-apps",
            "--disable-background-networking",
            "--disable-breakpad",
            "--no-pings",
        ]
        .map(String::from),
    );
    a.extend(blank);
    a
}

/// The browser's whole environment: its own home and temporary folder,
/// the language, and nothing of the desktop session.
fn browser_env(home: &Path, tmp: &Path) -> Vec<(String, String)> {
    let mut env = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("TMPDIR".to_string(), tmp.display().to_string()),
        (
            "XDG_CONFIG_HOME".to_string(),
            home.join(".config").display().to_string(),
        ),
        (
            "XDG_CACHE_HOME".to_string(),
            home.join(".cache").display().to_string(),
        ),
        (
            "XDG_DATA_HOME".to_string(),
            home.join(".local/share").display().to_string(),
        ),
        (
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        ),
    ];
    for k in ["LANG", "LANGUAGE", "LC_ALL", "TZ"] {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    #[cfg(windows)]
    env.extend(windows_env(home, tmp, |k| std::env::var(k).ok()));
    env
}

/// Windows' loader needs SystemRoot. No provider keys, session sockets or
/// account profile variables survive; browser state and temp stay here.
#[cfg(any(windows, test))]
fn windows_env(
    home: &Path,
    tmp: &Path,
    get: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<_> = [
        ("USERPROFILE", home.to_path_buf()),
        ("APPDATA", home.join("AppData/Roaming")),
        ("LOCALAPPDATA", home.join("AppData/Local")),
        ("TEMP", tmp.to_path_buf()),
        ("TMP", tmp.to_path_buf()),
    ]
    .into_iter()
    .map(|(k, p)| (k.to_string(), p.display().to_string()))
    .collect();
    for k in ["SystemRoot", "WINDIR"] {
        if let Some(v) = get(k) {
            out.push((k.to_string(), v));
        }
    }
    out
}

/// Downloads still named by their id were never finished.
/// The size of the files directly in `dir`.
fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn clean_partial(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let guid_like = n.len() == 36
            && n.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && n.matches('-').count() == 4;
        if guid_like {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 1..1000 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}{ext}", crate::util::now_ms()))
}

/// Write checked commands to the browser, in order.
#[cfg(any(unix, windows))]
async fn write_pipe(writer: PipeWriter, mut rx: queue::Receiver<Vec<u8>>, stop: CancellationToken) {
    use tokio::io::AsyncWriteExt;
    let mut writer = writer;
    loop {
        let msg = tokio::select! {
            m = rx.recv() => m,
            _ = stop.cancelled() => None,
        };
        let Some(mut item) = msg else { break };
        item.value.push(0);
        tokio::select! {
            _ = stop.cancelled() => break,
            result = writer.write_all(&item.value) => if result.is_err() { break; },
        }
    }
}

// ---- Landlock ------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod confine {
    //! The browser's Landlock domain: read the system, write only its own
    //! folders. Everything is prepared before the fork; `apply` only makes
    //! system calls.

    use std::os::fd::{AsRawFd, OwnedFd};
    use std::path::{Path, PathBuf};

    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: u32 = 1;
    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const REFER: u64 = 1 << 13;
    const TRUNCATE: u64 = 1 << 14;
    const IOCTL_DEV: u64 = 1 << 15;
    /// Rights that apply to a file (not only to directories).
    const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneath {
        allowed_access: u64,
        parent_fd: i32,
    }

    pub struct Plan {
        handled: u64,
        rules: Vec<(OwnedFd, u64)>,
    }

    /// The Landlock ABI this kernel has (0: none).
    pub fn abi() -> i32 {
        // SAFETY: the version query takes no attributes.
        let v = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        if v < 0 { 0 } else { v as i32 }
    }

    /// A path descriptor at 10 or above: the browser's start puts its pipes
    /// on 3 and 4 before the domain is entered, and must not clobber one of
    /// these (that failed the start now and then, "bad state").
    fn open_path(p: &Path) -> Option<OwnedFd> {
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(p)
            .ok()?;
        // SAFETY: fcntl on a descriptor we own; the copy is ours alone.
        let high = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
        (high >= 0).then(|| unsafe { OwnedFd::from_raw_fd(high) })
    }

    /// The domain for `bin` keeping its things in `own` (None: no Landlock
    /// here).
    pub fn plan(bin: &Path, own: &Path) -> Option<Plan> {
        plan_with(bin, own, &[])
    }

    /// [`plan`], and `extra` files it may read (a display's credentials).
    pub fn plan_with(bin: &Path, own: &Path, extra: &[PathBuf]) -> Option<Plan> {
        let abi = abi();
        if abi < 1 {
            return None;
        }
        let mut handled: u64 = (1 << 13) - 1;
        if abi >= 2 {
            handled |= REFER;
        }
        if abi >= 3 {
            handled |= TRUNCATE;
        }
        if abi >= 5 {
            handled |= IOCTL_DEV;
        }
        let read = READ_FILE | READ_DIR;
        let mut rules: Vec<(PathBuf, u64)> = Vec::new();
        // The system: programs, libraries, settings, fonts.
        for d in [
            "/usr",
            "/lib",
            "/lib32",
            "/lib64",
            "/libx32",
            "/bin",
            "/sbin",
            "/opt",
            "/etc",
            "/nix/store",
            "/gnu/store",
            "/run/current-system",
            "/var/cache/fontconfig",
            "/var/lib/dbus",
        ] {
            rules.push((PathBuf::from(d), read | EXECUTE));
        }
        // The browser's own folder (Chrome for Testing lives in the home folder).
        if let Some(dir) = std::fs::canonicalize(bin)
            .ok()
            .and_then(|b| b.parent().map(Path::to_path_buf))
        {
            rules.push((dir, read | EXECUTE));
        }
        // Name resolution may point outside /etc (/run/systemd/resolve…).
        if let Ok(r) = std::fs::canonicalize("/etc/resolv.conf")
            && let Some(dir) = r.parent()
        {
            rules.push((dir.to_path_buf(), read));
        }
        // Processes: its sandbox writes its own uid map and OOM scores.
        rules.push((PathBuf::from("/proc"), read | WRITE_FILE));
        rules.push((PathBuf::from("/sys"), read));
        // Shared memory between its processes, and a few devices.
        rules.push((PathBuf::from("/dev/shm"), handled));
        for f in [
            "/dev/null",
            "/dev/zero",
            "/dev/urandom",
            "/dev/random",
            "/dev/full",
        ] {
            rules.push((PathBuf::from(f), READ_FILE | WRITE_FILE));
        }
        // Its own folder: everything.
        rules.push((own.to_path_buf(), handled));
        for f in extra {
            rules.push((f.clone(), READ_FILE));
        }
        let rules = rules
            .into_iter()
            .filter_map(|(p, access)| {
                let meta = std::fs::metadata(&p).ok()?;
                let access = if meta.is_dir() {
                    access & handled
                } else {
                    access & handled & FILE_RIGHTS
                };
                open_path(&p).map(|fd| (fd, access))
            })
            .collect();
        Some(Plan { handled, rules })
    }

    impl Plan {
        /// Enter the domain (in the child, just before the program).
        pub fn apply(&self) -> std::io::Result<()> {
            let check = |r: libc::c_long| {
                if r < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(r)
                }
            };
            // SAFETY: plain system calls with valid arguments.
            unsafe {
                check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) as libc::c_long)?;
                let attr = RulesetAttr {
                    handled_access_fs: self.handled,
                };
                let ruleset = check(libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    &attr as *const RulesetAttr,
                    std::mem::size_of::<RulesetAttr>(),
                    0u32,
                ))? as i32;
                for (fd, access) in &self.rules {
                    let rule = PathBeneath {
                        allowed_access: *access,
                        parent_fd: fd.as_raw_fd(),
                    };
                    check(libc::syscall(
                        libc::SYS_landlock_add_rule,
                        ruleset,
                        RULE_PATH_BENEATH,
                        &rule as *const PathBeneath,
                        0u32,
                    ))?;
                }
                check(libc::syscall(
                    libc::SYS_landlock_restrict_self,
                    ruleset,
                    0u32,
                ))?;
                libc::close(ruleset);
            }
            Ok(())
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod confine {
    use std::path::{Path, PathBuf};

    pub struct Plan;

    impl Plan {
        pub fn apply(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub fn plan(_bin: &Path, _own: &Path) -> Option<Plan> {
        None
    }

    pub fn plan_with(_bin: &Path, _own: &Path, _extra: &[PathBuf]) -> Option<Plan> {
        None
    }
}

/// Whether this computer can keep the browser out of the home folder
/// (Linux with Landlock).
pub fn file_confinement() -> bool {
    #[cfg(target_os = "linux")]
    {
        confine::abi() >= 1
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_browser_environment_keeps_only_private_state_and_loader_paths() {
        let env = windows_env(Path::new("private/home"), Path::new("private/tmp"), |k| {
            Some(format!("system-{k}"))
        });
        let env: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(env["USERPROFILE"], "private/home");
        assert_eq!(
            env["APPDATA"],
            Path::new("private/home")
                .join("AppData/Roaming")
                .display()
                .to_string()
        );
        assert_eq!(
            env["LOCALAPPDATA"],
            Path::new("private/home")
                .join("AppData/Local")
                .display()
                .to_string()
        );
        assert_eq!(env["TEMP"], "private/tmp");
        assert_eq!(env["TMP"], "private/tmp");
        assert_eq!(env["SystemRoot"], "system-SystemRoot");
        assert_eq!(env.len(), 7);
        for k in [
            "OPENAI_API_KEY",
            "TESSERA_HOME",
            "DBUS_SESSION_BUS_ADDRESS",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!env.contains_key(k));
        }
    }

    #[test]
    fn nodes_report_webviews_separately_from_allowing_them() {
        assert_eq!(
            super::super::features().iter().any(|s| s == "webviews"),
            cfg!(any(unix, windows))
        );
        assert!(!NodeConfig::default().scope.webviews);
    }
    use tessera_proto::MachineScope;

    fn cmd(id: u64, method: &str, params: Value, session: Option<&str>) -> String {
        let mut v = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            v["sessionId"] = json!(s);
        }
        v.to_string()
    }

    /// A filter with one page attached (session "P1").
    fn filter() -> Filter {
        let mut f = Filter::new(MAX_TABS);
        f.learn(&json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "P1", "targetInfo": {"type": "page", "targetId": "T1"}, "waitingForDebugger": true}}));
        f
    }

    fn refused(f: &mut Filter, method: &str, params: Value, session: Option<&str>) -> String {
        match f.check(&cmd(7, method, params.clone(), session)) {
            Err(r) => r.reason,
            Ok(v) => panic!("{method} {params} passed: {v:?}"),
        }
    }

    fn passes(f: &mut Filter, method: &str, params: Value, session: Option<&str>) {
        match f.check(&cmd(7, method, params.clone(), session)) {
            Ok(Verdict::Pass(out)) => {
                let v: Value = serde_json::from_str(&out).unwrap();
                assert_eq!(v["method"], method);
                assert_eq!(v["id"], 7);
            }
            other => panic!("{method} {params} refused: {other:?}"),
        }
    }

    #[test]
    fn pdf_streams_are_bounded_page_owned_and_revoked() {
        let mut f = filter();
        passes(
            &mut f,
            "Page.printToPDF",
            json!({"transferMode":"ReturnAsStream"}),
            Some("P1"),
        );
        f.learn(&json!({"id":7,"result":{"stream":"pdf"}}));
        refused(&mut f, "IO.read", json!({"handle":"pdf","size":1}), None);
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","size":1}),
            Some("OTHER"),
        );
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","size":65537}),
            Some("P1"),
        );
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","offset":0,"size":1}),
            Some("P1"),
        );
        for _ in 0..256 {
            passes(
                &mut f,
                "IO.read",
                json!({"handle":"pdf","size":65536}),
                Some("P1"),
            );
        }
        passes(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","size":1}),
            Some("P1"),
        );
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","size":1}),
            Some("P1"),
        );
        passes(&mut f, "IO.close", json!({"handle":"pdf"}), Some("P1"));
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"pdf","size":1}),
            Some("P1"),
        );
        refused(
            &mut f,
            "Page.printToPDF",
            json!({"transferMode":"ReturnAsBase64"}),
            Some("P1"),
        );
        passes(
            &mut f,
            "Page.printToPDF",
            json!({"transferMode":"ReturnAsStream"}),
            Some("P1"),
        );
        f.learn(&json!({"id":7,"result":{"stream":"detached-pdf"}}));
        f.learn(&json!({"method":"Target.detachedFromTarget","params":{"sessionId":"P1"}}));
        assert!(f.pdf_streams.is_empty());
        refused(
            &mut f,
            "IO.read",
            json!({"handle":"detached-pdf","size":1}),
            Some("P1"),
        );
        // A pending print cannot add a new handle after its page was detached.
        f.pdfs.insert(8, "P2".into());
        f.learn(&json!({"method":"Target.detachedFromTarget","params":{"sessionId":"P2"}}));
        f.learn(&json!({"id":8,"result":{"stream":"late-pdf"}}));
        assert!(!f.pdf_streams.contains_key("late-pdf"));
    }
    #[test]
    fn local_files_and_browser_pages_are_never_opened() {
        let mut f = filter();
        for url in [
            "file:///etc/passwd",
            "FILE:///home/me/.ssh/id_ed25519",
            "chrome://settings",
            "chrome://version",
            "devtools://devtools/bundled/inspector.html",
            "chrome-extension://abc/page.html",
            "view-source:https://example.com",
            "javascript:alert(1)",
            "blob:https://example.com/x",
            "filesystem:https://example.com/temporary/x",
            "data:application/pdf;base64,AAAA",
            "about:settings",
            "",
        ] {
            let why = refused(&mut f, "Page.navigate", json!({"url": url}), Some("P1"));
            assert!(
                why.contains("can't be opened") || why.contains("isn't an address"),
                "{url}: {why}"
            );
        }
        for url in [
            "https://example.com/",
            "http://192.168.1.1/",
            "about:blank",
            "data:text/html;base64,PGgxPg==",
        ] {
            passes(&mut f, "Page.navigate", json!({"url": url}), Some("P1"));
        }
        // New tabs open blank, in the default profile only.
        refused(
            &mut f,
            "Target.createTarget",
            json!({"url": "file:///etc/hostname"}),
            None,
        );
        refused(
            &mut f,
            "Target.createTarget",
            json!({"url": "about:blank", "browserContextId": "X"}),
            None,
        );
        // Loading a resource: the web only, without the user's cookies.
        refused(
            &mut f,
            "Network.loadNetworkResource",
            json!({"frameId": "T1", "url": "file:///etc/passwd", "options": {"includeCredentials": false}}),
            Some("P1"),
        );
        refused(
            &mut f,
            "Network.loadNetworkResource",
            json!({"frameId": "T1", "url": "https://x.org/a", "options": {"includeCredentials": true}}),
            Some("P1"),
        );
        // No universal access for scripts, no DevTools console helpers.
        refused(
            &mut f,
            "Page.createIsolatedWorld",
            json!({"frameId": "T1", "worldName": "w", "grantUniveralAccess": true}),
            Some("P1"),
        );
        refused(
            &mut f,
            "Runtime.evaluate",
            json!({"expression": "1", "includeCommandLineAPI": true}),
            Some("P1"),
        );
        refused(
            &mut f,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "1", "includeCommandLineAPI": true}),
            Some("P1"),
        );
        // Pasting this computer's clipboard into a page.
        refused(
            &mut f,
            "Input.dispatchKeyEvent",
            json!({"type": "keyDown", "commands": ["paste"]}),
            Some("P1"),
        );
    }

    #[test]
    fn nothing_outside_the_page_sandbox_passes() {
        let mut f = filter();
        for (method, params, session) in [
            (
                "Browser.setDownloadBehavior",
                json!({"behavior": "allow", "downloadPath": "/home/me"}),
                None,
            ),
            (
                "Page.setDownloadBehavior",
                json!({"behavior": "allow", "downloadPath": "/home/me"}),
                Some("P1"),
            ),
            (
                "Browser.grantPermissions",
                json!({"permissions": ["clipboardReadWrite"]}),
                None,
            ),
            ("Browser.setPermission", json!({}), None),
            ("Browser.close", json!({}), None),
            ("Browser.crash", json!({}), None),
            ("Extensions.loadUnpacked", json!({"path": "/tmp/ext"}), None),
            ("Target.attachToBrowserTarget", json!({}), None),
            (
                "Target.attachToTarget",
                json!({"targetId": "T9", "flatten": true}),
                None,
            ),
            (
                "Target.exposeDevToolsProtocol",
                json!({"targetId": "T1"}),
                None,
            ),
            ("Target.createBrowserContext", json!({}), None),
            ("Target.setAutoAttach", json!({"autoAttach": true}), None),
            ("Target.setRemoteLocations", json!({"locations": []}), None),
            ("Target.getTargets", json!({}), None),
            (
                "DOM.setFileInputFiles",
                json!({"files": ["/etc/passwd"], "nodeId": 1}),
                Some("P1"),
            ),
            (
                "Input.dispatchDragEvent",
                json!({"type": "drop", "x": 1, "y": 1, "data": {"items": [], "files": ["/etc/passwd"]}}),
                Some("P1"),
            ),
            ("Page.printToPDF", json!({}), Some("P1")),
            ("Page.setBypassCSP", json!({"enabled": true}), Some("P1")),
            ("Network.getAllCookies", json!({}), Some("P1")),
            ("Network.getCookies", json!({}), Some("P1")),
            ("Storage.getCookies", json!({}), None),
            ("IO.resolveBlob", json!({"objectId": "x"}), None),
            ("SystemInfo.getProcessInfo", json!({}), None),
            ("Tracing.start", json!({}), None),
            (
                "Fetch.fulfillRequest",
                json!({"requestId": "1", "responseCode": 200}),
                Some("P1"),
            ),
            (
                "Fetch.enable",
                json!({"handleAuthRequests": true}),
                Some("P1"),
            ),
            ("Emulation.setGeolocationOverride", json!({}), Some("P1")),
            (
                "Security.setIgnoreCertificateErrors",
                json!({"ignore": true}),
                None,
            ),
            ("Runtime.compileScript", json!({}), Some("P1")),
        ] {
            refused(&mut f, method, params, session);
        }
        // Streams only as handed out.
        refused(&mut f, "IO.read", json!({"handle": "42"}), None);
        // Commands for a page this machine didn't attach, or none.
        refused(
            &mut f,
            "Runtime.evaluate",
            json!({"expression": "1"}),
            Some("OTHER"),
        );
        refused(&mut f, "Runtime.evaluate", json!({"expression": "1"}), None);
        refused(
            &mut f,
            "Target.closeTarget",
            json!({"targetId": "T1"}),
            Some("P1"),
        );
        // Unknown parameters on checked commands.
        refused(
            &mut f,
            "Page.reload",
            json!({"scriptToEvaluateOnLoad": "x"}),
            Some("P1"),
        );
        // Ids in the machine's own range, or none.
        assert!(
            f.check(&cmd(OWN_IDS + 1, "Browser.getVersion", json!({}), None))
                .is_err()
        );
        assert!(f.check(r#"{"method":"Browser.getVersion"}"#).is_err());
        assert!(f.check("not json").is_err());
        // A refusal answers the server's call with an error.
        let r = f
            .check(&cmd(9, "Browser.crash", json!({}), None))
            .unwrap_err();
        let a: Value = serde_json::from_str(&refusal(&r).unwrap()).unwrap();
        assert_eq!(a["id"], 9);
        assert!(
            a["error"]["message"]
                .as_str()
                .unwrap()
                .contains("refused by this machine")
        );
    }

    #[test]
    fn what_the_server_uses_passes_unchanged() {
        let mut f = filter();
        passes(&mut f, "Browser.getVersion", json!({}), None);
        passes(
            &mut f,
            "Target.createTarget",
            json!({"url": "about:blank", "background": true, "newWindow": true, "left": 64, "top": 64, "width": 1280, "height": 800}),
            None,
        );
        passes(
            &mut f,
            "Emulation.addScreen",
            json!({"left": 40000, "top": 0, "width": 6000, "height": 4000, "devicePixelRatio": 1.5, "label": "tessera-150"}),
            None,
        );
        passes(
            &mut f,
            "Browser.setWindowBounds",
            json!({"windowId": 3, "bounds": {"left": 1, "top": 2}}),
            None,
        );
        for (m, p) in [
            ("Runtime.runIfWaitingForDebugger", json!({})),
            ("Page.enable", json!({})),
            ("Page.bringToFront", json!({})),
            (
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": "1", "worldName": "tessera"}),
            ),
            // No security keys: in the page's own world, at once.
            (
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": crate::browser::NO_KEYS_JS, "runImmediately": true}),
            ),
            (
                "Runtime.addBinding",
                json!({"name": "__tsrFocus", "executionContextName": "tessera"}),
            ),
            (
                "Page.setInterceptFileChooserDialog",
                json!({"enabled": true}),
            ),
            (
                "Emulation.setFocusEmulationEnabled",
                json!({"enabled": true}),
            ),
            (
                "Emulation.setDeviceMetricsOverride",
                json!({"width": 1280, "height": 800, "deviceScaleFactor": 1, "mobile": false}),
            ),
            (
                "Page.startScreencast",
                json!({"format": "jpeg", "quality": 80, "maxWidth": 1280, "maxHeight": 800, "everyNthFrame": 1}),
            ),
            ("Page.screencastFrameAck", json!({"sessionId": 3})),
            (
                "Page.createIsolatedWorld",
                json!({"frameId": "T1", "worldName": "tessera", "grantUniveralAccess": false}),
            ),
            (
                "Runtime.evaluate",
                json!({"expression": "1", "contextId": 4, "returnByValue": true, "awaitPromise": true}),
            ),
            (
                "Input.dispatchMouseEvent",
                json!({"type": "mousePressed", "x": 1, "y": 2, "button": "left"}),
            ),
            (
                "Input.dispatchKeyEvent",
                json!({"type": "keyDown", "key": "a", "text": "a"}),
            ),
            ("Input.insertText", json!({"text": "hello"})),
            (
                "Fetch.enable",
                json!({"patterns": [{"urlPattern": "*://127.0.0.1:7878/*"}]}),
            ),
            (
                "Network.loadNetworkResource",
                json!({"frameId": "T1", "url": "https://example.com/favicon.ico", "options": {"disableCache": false, "includeCredentials": false}}),
            ),
        ] {
            passes(&mut f, m, p, Some("P1"));
        }
        // The load's stream may then be read, once handed out, and closed.
        f.learn(&json!({"id": 7, "result": {"resource": {"success": true, "stream": "S5"}}}));
        passes(
            &mut f,
            "IO.read",
            json!({"handle": "S5", "size": 16384}),
            None,
        );
        passes(&mut f, "IO.close", json!({"handle": "S5"}), None);
        refused(&mut f, "IO.read", json!({"handle": "S5"}), None);
        // Only the checked fields go on.
        let out = match f
            .check(r#"{"id":3,"method":"Browser.getVersion","params":{},"extra":"x"}"#)
            .unwrap()
        {
            Verdict::Pass(s) => s,
            v => panic!("{v:?}"),
        };
        assert!(!out.contains("extra"), "{out}");
        // This machine's own.
        assert!(matches!(
            f.check(&cmd(5, "Tessera.act", json!({"action": "click"}), None)),
            Ok(Verdict::Own(5, Own::Act(_)))
        ));
    }

    #[test]
    fn held_navigation_can_continue_without_request_overrides_or_other_page_access() {
        let mut f = filter();
        passes(
            &mut f,
            "Fetch.continueRequest",
            json!({"requestId":"held"}),
            Some("P1"),
        );
        passes(&mut f, "Fetch.disable", json!({}), Some("P1"));
        for p in [
            json!({"requestId":"held","url":"http://localhost/"}),
            json!({"requestId":"held","headers":[]}),
            json!({"requestId":"held","postData":"secret"}),
            json!({"requestId":""}),
        ] {
            refused(&mut f, "Fetch.continueRequest", p, Some("P1"));
        }
        refused(
            &mut f,
            "Fetch.continueRequest",
            json!({"requestId":"held"}),
            Some("other"),
        );
        refused(&mut f, "Fetch.disable", json!({}), Some("other"));
    }
    #[test]
    fn only_inert_uncacheable_document_can_bootstrap_a_known_page() {
        let mut f = filter();
        let blank = crate::browser::cdp::inert_document_response("held");
        passes(&mut f, "Fetch.fulfillRequest", blank.clone(), Some("P1"));
        refused(&mut f, "Fetch.fulfillRequest", blank.clone(), Some("other"));
        for (name, value) in [
            ("body", json!("PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==")),
            ("responseCode", json!(302)),
            (
                "responseHeaders",
                json!([{"name":"Set-Cookie","value":"x=y"}]),
            ),
            ("requestId", json!("")),
            ("binaryResponseHeaders", json!("anything")),
        ] {
            let mut changed = blank.clone();
            changed[name] = value;
            refused(&mut f, "Fetch.fulfillRequest", changed, Some("P1"));
        }
    }
    #[test]
    fn chunked_files_need_the_current_picker_and_pdf_cannot_read_host_files() {
        let mut f = Filter::new(MAX_TABS);
        f.pages.insert("P1".into());
        let params = json!({"backendNodeId":12,"owner":"device:picker","action":{"type":"start","files":[{"name":"homework.pdf","size":20<<20}]}});
        assert!(
            f.check(&cmd(8, "Tessera.uploadFiles", params.clone(), Some("P1")))
                .is_err()
        );
        f.learn(&json!({"method":"Page.fileChooserOpened","sessionId":"P1","params":{"backendNodeId":12,"mode":"selectSingle"}}));
        assert!(matches!(
            f.check(&cmd(8, "Tessera.uploadFiles", params.clone(), Some("P1"))),
            Ok(Verdict::Own(8, Own::UploadFiles { node: 12, .. }))
        ));
        let mut wrong = params;
        wrong["backendNodeId"] = json!(13);
        assert!(
            f.check(&cmd(8, "Tessera.uploadFiles", wrong, Some("P1")))
                .is_err()
        );
        assert!(
            f.check(&cmd(
                8,
                "Page.printToPDF",
                json!({"transferMode":"ReturnAsStream","printBackground":true}),
                Some("P1")
            ))
            .is_ok()
        );
        assert!(
            f.check(&cmd(
                8,
                "Page.printToPDF",
                json!({"transferMode":"ReturnAsBase64"}),
                Some("P1")
            ))
            .is_err()
        );
        assert!(
            f.check(&cmd(
                8,
                "Page.printToPDF",
                json!({"transferMode":"ReturnAsBase64","headerTemplate":"file:///etc/passwd"}),
                Some("P1")
            ))
            .is_err()
        );
    }
    #[test]
    fn selected_file_bytes_need_a_current_chooser_not_a_host_path() {
        let mut f = Filter::new(MAX_TABS);
        f.pages.insert("P1".into());
        let files = json!([{"name":"homework.txt","mime":"text/plain","data":"aG9tZXdvcms="}]);
        refused(
            &mut f,
            "Tessera.chooseFiles",
            json!({"backendNodeId":12,"files":files}),
            Some("P1"),
        );
        f.learn(&json!({"method":"Page.fileChooserOpened","sessionId":"P1","params":{"backendNodeId":12,"mode":"selectSingle"}}));
        assert!(matches!(
            f.check(&cmd(
                8,
                "Tessera.chooseFiles",
                json!({"backendNodeId":12,"files":files}),
                Some("P1")
            )),
            Ok(Verdict::Own(8, Own::ChooseFiles { node: 12, .. }))
        ));
        for (method, params) in [
            (
                "DOM.setFileInputFiles",
                json!({"backendNodeId":12,"files":["/etc/passwd"]}),
            ),
            (
                "Tessera.chooseFiles",
                json!({"backendNodeId":12,"files":["/etc/passwd"]}),
            ),
            (
                "Tessera.chooseFiles",
                json!({"backendNodeId":13,"files":files}),
            ),
            (
                "Tessera.chooseFiles",
                json!({"backendNodeId":12,"files":files,"path":"/etc/passwd"}),
            ),
        ] {
            refused(&mut f, method, params, Some("P1"));
        }
        f.learn(&json!({"method":"Page.frameNavigated","sessionId":"P1","params":{"frame":{}}}));
        refused(
            &mut f,
            "Tessera.chooseFiles",
            json!({"backendNodeId":12,"files":files}),
            Some("P1"),
        );
    }
    #[test]
    fn agent_upload_commands_need_actor_picker_and_bounded_sources() {
        let mut f = Filter::new(MAX_TABS);
        f.pages.insert("P1".into());
        let actor =
            json!({"session":"chat","action":"upload","mode":"default","uploadAsk":"risky"});
        let read = json!({"paths":["/notes.txt"],"actor":actor});
        refused(&mut f, "Tessera.readUpload", read.clone(), Some("P1"));
        f.learn(&json!({"method":"Page.fileChooserOpened","sessionId":"P1","params":{"backendNodeId":12,"mode":"selectSingle"}}));
        assert!(matches!(
            f.check(&cmd(8, "Tessera.readUpload", read, Some("P1"))),
            Ok(Verdict::Own(8, Own::AgentUpload { node: None, .. }))
        ));
        let sources = json!({"paths":["/notes.txt"],"files":[{"name":"notes.txt","mime":"text/plain","data":"bm90ZXM="}]});
        let send = json!({"backendNodeId":12,"sources":sources,"actor":actor});
        assert!(matches!(
            f.check(&cmd(
                9,
                "Tessera.agentChooseFiles",
                send.clone(),
                Some("P1")
            )),
            Ok(Verdict::Own(9, Own::AgentUpload { node: Some(12), .. }))
        ));
        for (method, params) in [
            ("Tessera.readUpload", json!({"paths":["/notes.txt"]})),
            (
                "Tessera.readUpload",
                json!({"paths":["/notes.txt"],"actor":null}),
            ),
            (
                "Tessera.readUpload",
                json!({"paths":["/notes.txt"],"actor":{}}),
            ),
            ("Tessera.readUpload", json!({"actor":actor})),
            (
                "Tessera.agentChooseFiles",
                json!({"sources":sources,"actor":actor}),
            ),
            (
                "Tessera.agentChooseFiles",
                json!({"backendNodeId":12,"actor":actor}),
            ),
            ("DOM.describeNode", json!({})),
            ("DOM.describeNode", json!({"backendNodeId":12})),
            ("Tessera.readUpload", json!({"paths":[],"actor":actor})),
            (
                "Tessera.readUpload",
                json!({"paths":["/notes.txt"],"actor":{"session":"chat","action":"click"}}),
            ),
            (
                "Tessera.agentChooseFiles",
                json!({"backendNodeId":13,"sources":sources,"actor":actor}),
            ),
            (
                "Tessera.agentChooseFiles",
                json!({"backendNodeId":12,"sources":sources,"actor":actor,"files":["/notes.txt"]}),
            ),
            ("DOM.describeNode", json!({"backendNodeId":13,"depth":0})),
            ("DOM.describeNode", json!({"backendNodeId":12,"depth":1})),
        ] {
            refused(&mut f, method, params, Some("P1"));
        }
        passes(
            &mut f,
            "DOM.describeNode",
            json!({"backendNodeId":12,"depth":0}),
            Some("P1"),
        );
        assert!(changes("upload"));
        let config = config(|s| s.webviews = true);
        assert!(
            may_act(
                &config,
                false,
                &json!({"session":"chat","action":"upload","mode":"read_only"})
            )
            .is_err()
        );
        f.learn(&json!({"method":"Page.frameNavigated","sessionId":"P1","params":{"frame":{}}}));
        refused(&mut f, "Tessera.agentChooseFiles", send, Some("P1"));
    }
    #[test]
    fn tabs_are_capped_and_detached_pages_forgotten() {
        let mut f = Filter::new(2);
        f.learn(&json!({"method": "Target.attachedToTarget", "params": {"sessionId": "P1", "targetInfo": {"type": "page", "targetId": "T1"}}}));
        passes(
            &mut f,
            "Target.createTarget",
            json!({"url": "about:blank"}),
            None,
        );
        let why = refused(
            &mut f,
            "Target.createTarget",
            json!({"url": "about:blank"}),
            None,
        );
        assert!(why.contains("at most 2 tabs"), "{why}");
        // Its answer: one fewer on the way.
        f.learn(&json!({"id": 7, "result": {"targetId": "T2"}}));
        f.learn(&json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "P1", "targetId": "T1"}}));
        assert!(!f.pages.contains("P1"));
        passes(
            &mut f,
            "Target.createTarget",
            json!({"url": "about:blank"}),
            None,
        );
        refused(&mut f, "Page.enable", json!({}), Some("P1"));
        // Pages past the hard limit (popups) close at once.
        let mut g = Filter::new(MAX_TABS);
        for i in 0..MAX_PAGES {
            assert!(g.learn(&json!({"method": "Target.attachedToTarget", "params": {"sessionId": format!("S{i}"), "targetInfo": {"type": "page", "targetId": format!("T{i}")}}})).is_none());
        }
        assert_eq!(
            g.learn(&json!({"method": "Target.attachedToTarget", "params": {"sessionId": "Sx", "targetInfo": {"type": "page", "targetId": "Tx"}}})),
            Some("Tx".to_string())
        );
        // The browser's own pages (opened by a shortcut) close at once, and
        // so does a page that ends up on one; web pages stay.
        let mut f = Filter::new(MAX_TABS);
        for url in [
            "chrome://settings/",
            "chrome://history/",
            "devtools://devtools/x",
            "file:///etc/hostname",
        ] {
            let t = f.learn(&json!({"method": "Target.attachedToTarget", "params": {"sessionId": "S9", "targetInfo": {"type": "page", "targetId": "T9", "url": url}}}));
            assert_eq!(t.as_deref(), Some("T9"), "{url}");
            assert!(!f.pages.contains("S9"));
        }
        for url in [
            "",
            "about:blank",
            "https://example.com/",
            "chrome-error://chromewebdata/",
        ] {
            assert!(f.learn(&json!({"method": "Target.attachedToTarget", "params": {"sessionId": "S1", "targetInfo": {"type": "page", "targetId": "T1", "url": url}}})).is_none(), "{url}");
        }
        assert_eq!(
            f.learn(&json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": {"type": "page", "targetId": "T1", "url": "chrome://extensions/"}}})).as_deref(),
            Some("T1")
        );
        assert!(f.learn(&json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": {"type": "page", "targetId": "T1", "url": "https://x.org/"}}})).is_none());
    }

    fn config(f: impl FnOnce(&mut MachineScope)) -> NodeConfig {
        let mut c = NodeConfig {
            enabled: true,
            ..Default::default()
        };
        c.scope.webviews = true;
        f(&mut c.scope);
        c
    }

    #[test]
    fn agents_act_only_as_this_machine_allows() {
        let act = |bot: Option<(&str, &str)>, mode: &str, action: &str| {
            json!({"action": action, "mode": mode, "chat": "Chat",
                   "bot": bot.map(|(id, h)| json!({"id": id, "handle": h, "name": h}))})
        };
        let any = config(|_| {});
        assert!(may_act(&any, false, &act(None, "auto", "click")).is_ok());
        assert!(may_act(&any, false, &act(Some(("b1", "sweeper")), "ask", "type")).is_ok());
        // Only certain bots.
        let only = config(|s| s.bots = Some(vec!["sweeper".into()]));
        assert!(may_act(&only, false, &act(Some(("b1", "sweeper")), "auto", "click")).is_ok());
        let why = may_act(&only, false, &act(Some(("b2", "writer")), "auto", "click")).unwrap_err();
        assert!(why.contains("only certain bots"), "{why}");
        assert!(may_act(&only, false, &act(None, "auto", "snapshot")).is_err());
        // Paused, web views off, turned off.
        assert!(
            may_act(&any, true, &act(None, "auto", "snapshot"))
                .unwrap_err()
                .contains("paused")
        );
        let off = config(|s| s.webviews = false);
        assert!(
            may_act(&off, false, &act(None, "auto", "snapshot"))
                .unwrap_err()
                .contains("off")
        );
        let mut disabled = any.clone();
        disabled.enabled = false;
        assert!(may_act(&disabled, false, &act(None, "auto", "snapshot")).is_err());
        // A turn from before the user's latest Stop here: refused, whatever
        // it asks; one from after it (or from an older server) as usual.
        let resumed = config(|s| s.stops = 1);
        let mut old = act(None, "auto", "snapshot");
        old["stops"] = json!(0);
        assert!(
            may_act(&resumed, false, &old)
                .unwrap_err()
                .contains("before the user's latest Stop")
        );
        old["stops"] = json!(1);
        assert!(may_act(&resumed, false, &old).is_ok());
        assert!(may_act(&resumed, false, &act(None, "auto", "snapshot")).is_ok());
        // A read-only chat reads, and changes nothing.
        assert!(may_act(&any, false, &act(None, "read_only", "snapshot")).is_ok());
        assert!(
            may_act(&any, false, &act(None, "read_only", "click"))
                .unwrap_err()
                .contains("read-only")
        );
        assert!(
            may_act(&any, false, &json!({"action": "type"})).is_err(),
            "no mode: read-only"
        );
    }

    #[test]
    fn the_browser_gets_no_desktop_session_and_no_port() {
        let a = args(Path::new("/p"), Some(1.5));
        assert!(a.contains(&"--remote-debugging-pipe".to_string()));
        assert!(a.contains(&"--disable-extensions".to_string()));
        assert_eq!(
            a.contains(&"--headless".to_string()),
            !cfg!(target_os = "linux")
        );
        assert_eq!(
            a.contains(&"--ozone-platform=headless".to_string()),
            cfg!(target_os = "linux")
        );
        // No security keys on this computer's web views either.
        assert!(a.contains(&"--disable-blink-features=WebAuth".to_string()));
        // Web views don't hide that they are automated.
        assert!(!a.iter().any(|f| f.contains("AutomationControlled")));
        assert!(!a.iter().any(|f| f.contains("remote-debugging-port")
            || f == "--no-sandbox"
            || f.contains("load-extension")));
        assert_eq!(a.last().map(String::as_str), Some("about:blank"));
        let env = browser_env(Path::new("/d/home"), Path::new("/d/tmp"));
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for k in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "SSH_AUTH_SOCK",
        ] {
            assert!(!keys.contains(&k), "{k}");
        }
        assert!(env.contains(&("HOME".into(), "/d/home".into())));
    }

    /// The window on the screen is an ordinary browser: no debugging pipe or
    /// port, nothing automated, its own profile, security keys still off,
    /// and of the desktop session only the display (no session bus).
    #[test]
    fn the_window_on_the_screen_is_an_ordinary_browser_with_only_the_display() {
        let a = screen_args(Path::new("/d/profile"), "https://example.com/login");
        assert_eq!(
            a.first().map(String::as_str),
            Some("--user-data-dir=/d/profile")
        );
        assert_eq!(
            a.last().map(String::as_str),
            Some("https://example.com/login")
        );
        assert!(a.contains(&"--disable-blink-features=WebAuth".to_string()));
        for f in &a {
            assert!(
                !f.contains("remote-debugging")
                    && !f.contains("headless")
                    && !f.contains("AutomationControlled")
                    && !f.contains("enable-automation")
                    && f != "--no-sandbox",
                "{f}"
            );
        }
        let session = super::super::desktop::Session {
            vars: [
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                ("XDG_CURRENT_DESKTOP", "Hyprland"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
            keep: vec![],
            bus: Some("unix:path=/run/user/1000/bus".into()),
            label: "Hyprland on Wayland".into(),
        };
        let env = screen_env(&session);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "XDG_CURRENT_DESKTOP"]
        );
    }

    /// Showing a page on the screen asks the same as web views: on here,
    /// not paused, a web address. The refusal goes back in plain words.
    #[tokio::test]
    async fn a_page_is_shown_on_the_screen_only_as_web_views_allow() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::at(tmp.path().join("node-home"));
        paths.ensure().unwrap();
        let (tx, mut ups) = mpsc::unbounded_channel();
        let link: Arc<dyn Link> = Arc::new(TestLink(tx));
        let nb = NodeBrowser::new(paths.clone());
        let off = config(|s| s.webviews = false);
        let on = config(|_| {});
        for (cfg, paused, url, want) in [
            (&off, false, "https://example.com/", "web views are off"),
            (&on, true, "https://example.com/", "paused"),
            (&on, false, "file:///etc/passwd", "web"),
        ] {
            nb.handle(
                &link,
                vec![NodeBrowserDown::Screen {
                    url: Some(url.into()),
                }],
                &Ctx {
                    config: cfg,
                    paused,
                },
            );
            match tokio::time::timeout(Duration::from_secs(5), ups.recv())
                .await
                .unwrap()
            {
                Some(NodeBrowserUp::Screen {
                    shown: false,
                    error: Some(e),
                }) => assert!(e.contains(want), "{want}: {e}"),
                other => panic!("{want}: {other:?}"),
            }
        }
        assert!(!dir(&paths).exists(), "nothing was set up");
        // Closing when nothing is open: just says it's closed.
        nb.handle(
            &link,
            vec![NodeBrowserDown::Screen { url: None }],
            &Ctx {
                config: &on,
                paused: false,
            },
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), ups.recv())
                .await
                .unwrap(),
            Some(NodeBrowserUp::Screen {
                shown: false,
                error: None
            })
        ));
    }

    /// After the user's Stop here (also one already resumed, before the
    /// machine's next look closes the browser), nothing more reaches a
    /// browser started before it: commands get an error, actions a refusal.
    #[test]
    fn a_browser_from_before_a_stop_gets_nothing_more() {
        let tmp = tempfile::tempdir().unwrap();
        let nb = NodeBrowser::new(Paths::at(tmp.path().join("node-home")));
        let mut up = nb.0.up.lock().rx.take().unwrap();
        let (tx, mut rx) = queue::channel(8 << 20, CancellationToken::new());
        *nb.0.run.lock() = Some(Run {
            run: 1,
            tx,
            filter: Filter::new(MAX_TABS),
            own: Arc::new(OwnCalls::default()),
            stop: CancellationToken::new(),
            asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_use: Arc::new(Mutex::new(Instant::now())),
            upload_finishes: HashMap::new(),
            uploads: crate::browser::upload::Uploads::new(tmp.path().join("uploads")),
            downloads: tmp.path().join("downloads"),
            names: HashMap::new(),
            saved: HashMap::new(),
            kept: 0,
            receiving: HashMap::new(),
            pid: None,
            #[cfg(windows)]
            job: None,
            stops: 0,
        });
        let send = |c: &NodeConfig| {
            nb.commands(
                1,
                vec![
                    cmd(3, "Browser.getVersion", json!({}), None),
                    cmd(
                        4,
                        "Tessera.act",
                        json!({"action": "click", "mode": "auto"}),
                        None,
                    ),
                ],
                &Ctx {
                    config: c,
                    paused: false,
                },
            );
        };
        let answers = |up: &mut queue::Receiver<NodeBrowserUp>| -> Vec<Value> {
            let mut v = Vec::new();
            while let Ok(m) = up.try_recv() {
                if let NodeBrowserUp::Cdp { messages, .. } = m.value {
                    v.extend(messages.iter().map(|s| serde_json::from_str(s).unwrap()));
                }
            }
            v
        };
        // As it started: through.
        send(&config(|_| {}));
        assert!(rx.try_recv().is_ok());
        assert!(
            answers(&mut up)
                .iter()
                .any(|a| a["id"] == 4 && a.get("result").is_some())
        );
        // Stopped and resumed since (the count moved): nothing reaches it.
        send(&config(|s| s.stops = 1));
        assert!(rx.try_recv().is_err());
        let got = answers(&mut up);
        for id in [3, 4] {
            assert!(
                got.iter().any(|a| a["id"] == id
                    && a["error"]["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("stopped"))),
                "{got:?}"
            );
        }
        // Stopped now: the same.
        send(&config(|s| s.stopped = true));
        assert!(rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_during_a_blocked_cdp_pipe_write_finishes_immediately() {
        let (writer, _blocked_reader) = tokio::net::unix::pipe::pipe().unwrap();
        let stop = CancellationToken::new();
        let (tx, rx) = queue::channel(8 << 20, stop.clone());
        tx.send(vec![b'x'; 1 << 20]).unwrap();
        let task = tokio::spawn(write_pipe(writer, rx, stop.clone()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!task.is_finished(), "pipe wasn't blocked");
        stop.cancel();
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn blocked_link_flood_ends_the_run_and_stop_and_disconnect_do_not_wait() {
        struct Blocked;
        impl Link for Blocked {
            fn call(
                &self,
                _: Request,
            ) -> futures::future::BoxFuture<'static, Result<Value, tessera_proto::ApiError>>
            {
                Box::pin(std::future::pending())
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let nb = NodeBrowser::new(Paths::at(tmp.path().join("node-home")));
        let link: Arc<dyn Link> = Arc::new(Blocked);
        nb.0.clone().forward_with(link.clone());
        let stop = nb.0.up.lock().stop.child_token();
        let (tx, _rx) = queue::channel(8 << 20, stop.clone());
        *nb.0.run.lock() = Some(Run {
            run: 1,
            tx,
            filter: Filter::new(MAX_TABS),
            own: Arc::new(OwnCalls::default()),
            stop: stop.clone(),
            asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_use: Arc::new(Mutex::new(Instant::now())),
            upload_finishes: HashMap::new(),
            uploads: crate::browser::upload::Uploads::new(tmp.path().join("uploads")),
            downloads: tmp.path().join("downloads"),
            names: HashMap::new(),
            saved: HashMap::new(),
            kept: 0,
            receiving: HashMap::new(),
            pid: None,
            #[cfg(windows)]
            job: None,
            stops: 0,
        });
        // Let the real forwarder hold leases in blocked RPCs, then flood it.
        for _ in 0..80 {
            if nb
                .0
                .up
                .lock()
                .tx
                .send(NodeBrowserUp::Cdp {
                    run: 1,
                    messages: vec!["x".repeat(1 << 20)],
                })
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::timeout(Duration::from_millis(100), stop.cancelled())
            .await
            .unwrap();
        let on = config(|_| {});
        tokio::time::timeout(Duration::from_millis(100), async {
            nb.handle(
                &link,
                vec![NodeBrowserDown::Stop { run: 1 }],
                &Ctx {
                    config: &on,
                    paused: false,
                },
            );
            nb.disconnected("test disconnected");
        })
        .await
        .unwrap();
        assert!(!nb.running());
        // Reconnect makes a fresh bounded queue, not a permanently dead sender.
        nb.connected();
        assert!(
            nb.0.up
                .lock()
                .tx
                .send(NodeBrowserUp::Screen {
                    shown: false,
                    error: None
                })
                .is_ok()
        );
        nb.disconnected("test ended");
    }

    /// A server/link fixture drops all pre-overflow RPCs and the first failure
    /// acknowledgement. Recovery must close sequence gaps and report failure
    /// before a fresh run can pass a checked CDP command and its reply.
    #[tokio::test]
    async fn overflow_reports_failure_and_a_fresh_view_works_without_reconnect() {
        #[derive(Default)]
        struct Server {
            open: std::sync::atomic::AtomicBool,
            drop_error: std::sync::atomic::AtomicBool,
            wake: tokio::sync::Notify,
            seen: Mutex<(u64, Vec<NodeBrowserUp>)>,
        }
        impl Link for Arc<Server> {
            fn call(
                &self,
                req: Request,
            ) -> futures::future::BoxFuture<'static, Result<Value, tessera_proto::ApiError>>
            {
                let server = self.clone();
                Box::pin(async move {
                    while !server.open.load(Ordering::SeqCst) {
                        server.wake.notified().await;
                    }
                    let Request::NodeBrowser { seq, msgs } = req else {
                        panic!("unexpected request");
                    };
                    if msgs
                        .iter()
                        .any(|m| matches!(m, NodeBrowserUp::Failed { .. }))
                        && server.drop_error.swap(false, Ordering::SeqCst)
                    {
                        return Err(tessera_proto::ApiError::new(
                            tessera_proto::ErrorCode::Internal,
                            "lost error ack",
                        ));
                    }
                    let mut seen = server.seen.lock();
                    if seq > seen.0 {
                        assert_eq!(seq, seen.0 + 1, "recovery left a sequence gap");
                        seen.0 = seq;
                        seen.1.extend(msgs);
                    }
                    Ok(Value::Null)
                })
            }
        }
        fn run(nb: &NodeBrowser, root: &Path, id: u64) -> queue::Receiver<Vec<u8>> {
            let stop = nb.0.up.lock().stop.child_token();
            let (tx, rx) = queue::channel(8 << 20, stop.clone());
            *nb.0.run.lock() = Some(Run {
                run: id,
                tx,
                filter: Filter::new(MAX_TABS),
                own: Arc::new(OwnCalls::default()),
                stop,
                asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last_use: Arc::new(Mutex::new(Instant::now())),
                upload_finishes: HashMap::new(),
                uploads: crate::browser::upload::Uploads::new(root.join("uploads")),
                downloads: root.join("downloads"),
                names: HashMap::new(),
                saved: HashMap::new(),
                kept: 0,
                receiving: HashMap::new(),
                pid: None,
                #[cfg(windows)]
                job: None,
                stops: 0,
            });
            rx
        }
        let tmp = tempfile::tempdir().unwrap();
        let nb = NodeBrowser::new(Paths::at(tmp.path().join("node-home")));
        let server = Arc::new(Server::default());
        server.drop_error.store(true, Ordering::SeqCst);
        let link: Arc<dyn Link> = Arc::new(server.clone());
        nb.0.clone().forward_with(link.clone());
        let _old_pipe = run(&nb, tmp.path(), 1);
        let old_stop = nb.0.up.lock().stop.clone();
        nb.0.send_up(NodeBrowserUp::Cdp {
            run: 1,
            messages: vec!["x".repeat(1 << 20)],
        })
        .unwrap();
        while nb.0.seq.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        for _ in 0..80 {
            if nb
                .0
                .send_up(NodeBrowserUp::Cdp {
                    run: 1,
                    messages: vec!["x".repeat(1 << 20)],
                })
                .is_err()
            {
                break;
            }
        }
        assert!(old_stop.is_cancelled());
        let on = config(|_| {});
        // A concurrent Start is refused reliably, never launched with a dead token.
        nb.start(
            2,
            None,
            &Ctx {
                config: &on,
                paused: false,
            },
        );
        server.open.store(true, Ordering::SeqCst);
        server.wake.notify_waiters();
        tokio::time::timeout(Duration::from_secs(2), async {
            while nb.0.up.lock().stop.is_cancelled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!nb.running());
        for expected in [1, 2] {
            assert!(server.seen.lock().1.iter().any(|m| matches!(m,
                NodeBrowserUp::Failed { run, error } if *run == expected
                    && error == "web view stopped: it produced more than the link could carry")));
        }
        assert!(server.seen.lock().1.iter().any(|m| matches!(m,
            NodeBrowserUp::Stopped { run: 1, reason: Some(reason), crashed: true }
                if reason == "web view stopped: it produced more than the link could carry")));
        assert!(
            !server.drop_error.load(Ordering::SeqCst),
            "failure wasn't retried"
        );
        // No connected(), manual reset or changed limits: exercise production
        // run, command filter, downlink and reply producer through the new queue.
        let mut pipe = run(&nb, tmp.path(), 3);
        nb.0.send_up(NodeBrowserUp::Started {
            run: 3,
            product: "CDP fixture".into(),
            speed_key: "test".into(),
            max_tabs: 8,
        })
        .unwrap();
        nb.commands(
            3,
            vec![cmd(7, "Browser.getVersion", json!({}), None)],
            &Ctx {
                config: &on,
                paused: false,
            },
        );
        let request = pipe.try_recv().unwrap();
        let request: Value = serde_json::from_slice(&request.value).unwrap();
        assert_eq!(request["method"], "Browser.getVersion");
        let reply = json!({"id":7,"result":{"product":"CDP fixture"}}).to_string();
        let reply_for_server =
            nb.0.on_message(3, reply.as_bytes(), &OwnCalls::default())
                .unwrap();
        // read_pipe batches the returned checked reply before publishing it.
        nb.0.send_up(NodeBrowserUp::Cdp {
            run: 3,
            messages: vec![reply_for_server],
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if server.seen.lock().1.iter().any(|m| {
                    matches!(m,
                    NodeBrowserUp::Cdp { run: 3, messages } if messages.contains(&reply))
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        nb.disconnected("test ended");
    }
    /// One download after another can't fill the disk: past the total
    /// for the downloads folder, a download is stopped.
    #[test]
    fn downloads_stop_when_the_folder_holds_enough() {
        let tmp = tempfile::tempdir().unwrap();
        let nb = NodeBrowser::new(Paths::at(tmp.path().join("node-home")));
        let (tx, mut rx) = queue::channel(8 << 20, CancellationToken::new());
        *nb.0.run.lock() = Some(Run {
            run: 1,
            tx,
            filter: Filter::new(MAX_TABS),
            own: Arc::new(OwnCalls::default()),
            stop: CancellationToken::new(),
            asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_use: Arc::new(Mutex::new(Instant::now())),
            upload_finishes: HashMap::new(),
            uploads: crate::browser::upload::Uploads::new(tmp.path().join("uploads")),
            downloads: tmp.path().join("downloads"),
            names: HashMap::new(),
            saved: HashMap::new(),
            kept: MAX_DOWNLOADS_TOTAL - (1 << 20),
            receiving: HashMap::new(),
            pid: None,
            #[cfg(windows)]
            job: None,
            stops: 0,
        });
        let own = OwnCalls::default();
        let progress = |guid: &str, got: u64| {
            json!({"method": "Browser.downloadProgress", "params": {"guid": guid, "state": "inProgress", "receivedBytes": got}})
                .to_string()
        };
        let cancels = |rx: &mut queue::Receiver<Vec<u8>>| {
            let mut v = Vec::new();
            while let Ok(m) = rx.try_recv() {
                let m: Value = serde_json::from_slice(&m.value).unwrap();
                if m["method"] == "Browser.cancelDownload" {
                    v.push(m["params"]["guid"].as_str().unwrap().to_string());
                }
            }
            v
        };
        nb.0.on_message(1, progress("a", 512 << 10).as_bytes(), &own);
        assert!(cancels(&mut rx).is_empty(), "room for half a megabyte");
        nb.0.on_message(1, progress("b", 600 << 10).as_bytes(), &own);
        assert_eq!(cancels(&mut rx), ["b"], "together past the total");
    }

    #[tokio::test]
    async fn only_this_computer_turns_web_views_on() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::at(tmp.path().join("node-home"));
        paths.ensure().unwrap();
        let (tx, mut ups) = mpsc::unbounded_channel();
        let link: Arc<dyn Link> = Arc::new(TestLink(tx));
        let nb = NodeBrowser::new(paths.clone());
        // Off here: the server asking to start it changes nothing.
        let off = config(|s| s.webviews = false);
        nb.handle(
            &link,
            vec![NodeBrowserDown::Start {
                run: 1,
                scale: None,
            }],
            &Ctx {
                config: &off,
                paused: false,
            },
        );
        assert!(matches!(
            ups.recv().await,
            Some(NodeBrowserUp::Stopped {
                run: 1,
                crashed: false,
                ..
            })
        ));
        match tokio::time::timeout(Duration::from_secs(5), ups.recv())
            .await
            .unwrap()
        {
            Some(NodeBrowserUp::Failed { run: 1, error }) => {
                assert!(
                    error.contains("web views are off on this machine")
                        && error.contains("only this computer"),
                    "{error}"
                )
            }
            other => panic!("{other:?}"),
        }
        assert!(!nb.running());
        assert!(!dir(&paths).exists(), "nothing was set up");
        // Nor while paused, or turned off.
        let on = config(|_| {});
        nb.handle(
            &link,
            vec![NodeBrowserDown::Start {
                run: 2,
                scale: None,
            }],
            &Ctx {
                config: &on,
                paused: true,
            },
        );
        assert!(matches!(
            ups.recv().await,
            Some(NodeBrowserUp::Stopped {
                run: 2,
                crashed: false,
                ..
            })
        ));
        assert!(matches!(
            ups.recv().await,
            Some(NodeBrowserUp::Failed { run: 2, .. })
        ));
        let mut disabled = on.clone();
        disabled.enabled = false;
        nb.handle(
            &link,
            vec![NodeBrowserDown::Start {
                run: 3,
                scale: None,
            }],
            &Ctx {
                config: &disabled,
                paused: false,
            },
        );
        assert!(matches!(
            ups.recv().await,
            Some(NodeBrowserUp::Stopped {
                run: 3,
                crashed: false,
                ..
            })
        ));
        assert!(matches!(
            ups.recv().await,
            Some(NodeBrowserUp::Failed { run: 3, .. })
        ));
        // Commands for a browser that isn't running go nowhere.
        nb.handle(
            &link,
            vec![NodeBrowserDown::Cdp {
                run: 1,
                messages: vec![cmd(1, "Browser.getVersion", json!({}), None)],
            }],
            &Ctx {
                config: &on,
                paused: false,
            },
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), ups.recv())
                .await
                .is_err()
        );
        // The log says so.
        let log = std::fs::read_to_string(super::super::log_file(&paths)).unwrap();
        assert!(log.contains("web views are off on this machine"), "{log}");
    }

    // ---- with a real browser ----------------------------------------------

    /// The server's side of the tunnel, in a test: commands in, answers out.
    struct Rig {
        nb: NodeBrowser,
        link: Arc<dyn Link>,
        ups: mpsc::UnboundedReceiver<NodeBrowserUp>,
        config: NodeConfig,
        next: u64,
        /// Events seen while waiting for answers.
        events: Vec<Value>,
    }

    struct TestLink(mpsc::UnboundedSender<NodeBrowserUp>);

    impl Link for TestLink {
        fn call(
            &self,
            req: Request,
        ) -> futures::future::BoxFuture<'static, Result<Value, tessera_proto::ApiError>> {
            if let Request::NodeBrowser { msgs, .. } = req {
                for m in msgs {
                    let _ = self.0.send(m);
                }
            }
            Box::pin(async { Ok(Value::Null) })
        }
    }

    impl Rig {
        async fn start(paths: Paths) -> Option<Self> {
            crate::browser::process::find(None)?;
            let (tx, ups) = mpsc::unbounded_channel();
            let mut rig = Rig {
                nb: NodeBrowser::new(paths),
                link: Arc::new(TestLink(tx)),
                ups,
                config: config(|_| {}),
                next: 1,
                events: Vec::new(),
            };
            rig.send(NodeBrowserDown::Start {
                run: 1,
                scale: None,
            });
            loop {
                match tokio::time::timeout(Duration::from_secs(40), rig.ups.recv()).await {
                    Ok(Some(NodeBrowserUp::Started { .. })) => return Some(rig),
                    // What it saw while starting.
                    Ok(Some(NodeBrowserUp::Cdp { messages, .. })) => rig
                        .events
                        .extend(messages.iter().filter_map(|m| serde_json::from_str(m).ok())),
                    other => panic!("the browser didn't start: {other:?}"),
                }
            }
        }

        fn send(&self, m: NodeBrowserDown) {
            let ctx = Ctx {
                config: &self.config,
                paused: false,
            };
            self.nb.handle(&self.link, vec![m], &ctx);
        }

        /// A command through the allowlist (`raw`: straight to the browser,
        /// as if the allowlist let anything through).
        async fn call(
            &mut self,
            method: &str,
            params: Value,
            session: Option<&str>,
            raw: bool,
        ) -> Result<Value, String> {
            let id = self.next;
            self.next += 1;
            let msg = cmd(id, method, params, session);
            if raw {
                let run = self.nb.0.run.lock();
                run.as_ref().unwrap().tx.send(msg.into_bytes()).unwrap();
            } else {
                self.send(NodeBrowserDown::Cdp {
                    run: 1,
                    messages: vec![msg],
                });
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                let up = tokio::time::timeout_at(deadline, self.ups.recv())
                    .await
                    .map_err(|_| format!("no answer to {method}"))?
                    .ok_or("closed")?;
                let NodeBrowserUp::Cdp { messages, .. } = up else {
                    continue;
                };
                let mut found = None;
                for m in messages {
                    let v: Value = serde_json::from_str(&m).unwrap();
                    if v["id"] == id {
                        found = Some(match v.get("error") {
                            Some(e) => Err(e["message"].as_str().unwrap_or_default().to_string()),
                            None => Ok(v["result"].clone()),
                        });
                    } else if v.get("method").is_some() {
                        self.events.push(v);
                    }
                }
                if let Some(r) = found {
                    return r;
                }
            }
        }

        /// A new page, ready (its session).
        async fn page(&mut self) -> String {
            let t = self
                .call(
                    "Target.createTarget",
                    json!({"url": "about:blank"}),
                    None,
                    false,
                )
                .await
                .unwrap();
            let target = t["targetId"].as_str().unwrap().to_string();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(e) = self.events.iter().find(|e| {
                    e["method"] == "Target.attachedToTarget"
                        && e["params"]["targetInfo"]["targetId"] == target.as_str()
                }) {
                    let s = e["params"]["sessionId"].as_str().unwrap().to_string();
                    self.call(
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        Some(&s),
                        false,
                    )
                    .await
                    .unwrap();
                    self.call("Page.enable", json!({}), Some(&s), false)
                        .await
                        .unwrap();
                    return s;
                }
                assert!(Instant::now() < deadline, "no page attached");
                let _ = self
                    .call("Browser.getVersion", json!({}), None, false)
                    .await;
            }
        }

        async fn text(&mut self, s: &str) -> String {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let r = self
                .call("Runtime.evaluate", json!({"expression": "location.href + ' | ' + (document.body ? document.body.innerText : '')", "returnByValue": true}), Some(s), false)
                .await
                .unwrap();
            r["result"]["value"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        }
    }

    #[tokio::test]
    async fn a_planned_memory_stop_flushes_persistent_cookies_before_restarting() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::at(tmp.path().canonicalize().unwrap().join("node-home"));
        paths.ensure().unwrap();
        let Some(mut rig) = Rig::start(paths.clone()).await else {
            return;
        };
        let page = rig.page().await;
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        // No account or site traffic: synthetic private profile only. The
        // raw test path is deliberate (agents cannot export/import cookies).
        let result = rig
            .call(
                "Network.setCookie",
                json!({
                    "name": "synthetic_remember_me", "value": "fixture_value",
                    "url": "https://fixture.invalid/", "secure": true,
                    "httpOnly": true, "expires": expires,
                }),
                Some(&page),
                true,
            )
            .await
            .unwrap();
        assert_eq!(result["success"], true);
        let run = rig.nb.0.run.lock().take().unwrap();
        run.save_and_stop().await;
        assert!(!rig.nb.running());
        let Some(mut again) = Rig::start(paths).await else {
            panic!("browser disappeared")
        };
        let cookies = again
            .call("Storage.getCookies", json!({}), None, true)
            .await
            .unwrap();
        assert!(
            cookies["cookies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "synthetic_remember_me"
                    && c["value"] == "fixture_value"
                    && c["httpOnly"] == true
                    && c["secure"] == true),
            "persistent sign-in cookie flushed"
        );
        again.nb.stop("the test ended");
    }

    /// Landlock alone (no allowlist in front): the browser opens a file in
    /// its own folder, and nothing outside it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn its_file_sandbox_keeps_it_out_of_the_home_folder() {
        let Some(bin) = crate::browser::process::find(None) else {
            eprintln!("no Chromium-family browser installed; skipping");
            return;
        };
        if !file_confinement() {
            eprintln!("no Landlock here; skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap().join("web-browser");
        let (profile, home, scratch) = (base.join("profile"), base.join("home"), base.join("tmp"));
        for d in [&base, &profile, &home, &scratch] {
            private_dir(d).unwrap();
        }
        let own = home.join("own.txt");
        std::fs::write(&own, "its own file").unwrap();
        let mine = tempfile::tempdir().unwrap();
        let secret = mine.path().canonicalize().unwrap().join("notes.txt");
        std::fs::write(&secret, "a private note").unwrap();
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args(args(&profile, None)).env_clear();
        for (k, v) in browser_env(&home, &scratch) {
            cmd.env(k, v);
        }
        let plan = confine::plan(&bin, &base).unwrap();
        let (mut child, (r, w)) =
            crate::browser::process::spawn_piped(&mut cmd, Some(Box::new(move || plan.apply())))
                .unwrap();
        let (cdp, _events) = crate::browser::cdp::Cdp::start(r, w);
        cdp.call(None, "Browser.getVersion", json!({}))
            .await
            .unwrap();
        let t = cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await
            .unwrap();
        let a = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": t["targetId"], "flatten": true}),
            )
            .await
            .unwrap();
        let session = a["sessionId"].as_str().unwrap().to_string();
        let read = async |path: &Path| {
            let s = Some(session.as_str());
            let _ = cdp
                .call(
                    s,
                    "Page.navigate",
                    json!({"url": format!("file://{}", path.display())}),
                )
                .await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            cdp.call(s, "Runtime.evaluate", json!({"expression": "document.body ? document.body.innerText : ''", "returnByValue": true}))
                .await
                .map(|v| v["result"]["value"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default()
        };
        assert!(
            read(&own).await.contains("its own file"),
            "its own folder is readable"
        );
        let text = read(&secret).await;
        assert!(!text.contains("a private note"), "{text}");
        kill_group(child.id());
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn the_browser_never_reads_files_outside_its_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let paths = Paths::at(root.join("node-home"));
        paths.ensure().unwrap();
        // A file of the user's, outside the browser's folder.
        let mine = tempfile::tempdir().unwrap();
        let secret = mine.path().canonicalize().unwrap().join("notes.txt");
        std::fs::write(&secret, "a private note").unwrap();
        let Some(mut rig) = Rig::start(paths.clone()).await else {
            eprintln!("no Chromium-family browser installed; skipping");
            return;
        };
        let s = rig.page().await;
        // It works: a page loads and runs.
        rig.call(
            "Page.navigate",
            json!({"url": "data:text/html,<p>hello from a page</p>"}),
            Some(&s),
            false,
        )
        .await
        .unwrap();
        assert!(rig.text(&s).await.contains("hello from a page"));
        // The allowlist refuses a local file…
        let url = format!("file://{}", secret.display());
        let e = rig
            .call("Page.navigate", json!({"url": url}), Some(&s), false)
            .await
            .unwrap_err();
        assert!(e.contains("refused by this machine"), "{e}");
        // …and should it ever let one through, the browser can't read it
        // (Landlock), where this computer has it.
        // …and should one ever get through, the page that shows it is
        // closed at once, and nothing of the file reaches the server.
        let _ = rig
            .call("Page.navigate", json!({"url": url}), Some(&s), true)
            .await;
        let closed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if rig.events.iter().any(|e| {
                    matches!(
                        e["method"].as_str(),
                        Some("Target.detachedFromTarget" | "Target.targetDestroyed")
                    )
                }) {
                    return;
                }
                let _ = rig.call("Browser.getVersion", json!({}), None, false).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        assert!(closed.is_ok(), "the page on the file was closed");
        assert!(
            !rig.events
                .iter()
                .any(|e| e.to_string().contains("a private note"))
        );
        let e = rig
            .call(
                "Runtime.evaluate",
                json!({"expression": "document.body.innerText", "returnByValue": true}),
                Some(&s),
                false,
            )
            .await
            .unwrap_err();
        assert!(e.contains("not a page this machine opened"), "{e}");
        // Its things are in Tessera's data folder, owner-only.
        let base = dir(&paths);
        assert!(base.join("profile").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        rig.nb.stop("the test ended");
        assert!(!rig.nb.running());
    }

    #[test]
    fn a_sudden_memory_jump_warns_before_stopping_and_can_recover() {
        let now = Instant::now();
        let mut since = None;
        assert!(
            memory_warn_due(80, 100, true),
            "Windows warns at soft threshold"
        );
        assert!(!memory_warn_due(79, 100, true));
        assert!(!memory_warn_due(80, 100, false));
        assert!(memory_warn_due(101, 100, false));
        assert!(!memory_stop_due(Some(80), 100, &mut since, now));
        assert!(!memory_stop_due(Some(101), 100, &mut since, now));
        assert!(!memory_stop_due(
            Some(101),
            100,
            &mut since,
            now + Duration::from_secs(4)
        ));
        assert!(memory_stop_due(
            Some(101),
            100,
            &mut since,
            now + Duration::from_secs(5)
        ));
        assert!(!memory_stop_due(
            Some(99),
            100,
            &mut since,
            now + Duration::from_secs(6)
        ));
        assert!(since.is_none());
        assert!(!memory_stop_due(
            Some(101),
            100,
            &mut since,
            now + Duration::from_secs(7)
        ));
        assert!(!memory_stop_due(
            None,
            100,
            &mut since,
            now + Duration::from_secs(12)
        ));
        assert!(since.is_none());
    }

    #[test]
    fn memory_is_counted_once_and_the_cap_follows_the_computer() {
        let rollup = "55d0a0000000-7ffd1b9f6000 ---p 00000000 00:00 0 [rollup]\nRss: 210400 kB\nPss: 79412 kB\nPss_Anon: 50000 kB\nShared_Clean: 120000 kB\n";
        assert_eq!(pss_of(rollup), Some(79_412 * 1024));
        assert_eq!(pss_of("Rss: 1 kB\n"), None);
        const GB: u64 = 1 << 30;
        // A quarter of the memory, between 1 and 8 GB: the user's 60 GB PC
        // gets 8 (it got 4, counting shared pages once per process).
        assert_eq!(cap_for(Some(60 * GB)), 8 * GB);
        assert_eq!(cap_for(Some(16 * GB)), 4 * GB);
        assert_eq!(cap_for(Some(2 * GB)), GB);
        assert_eq!(cap_for(None), 2 * GB);
        // Near the limit, or the computer short of memory: a page goes.
        let cap = 8 * GB;
        assert!(!should_shed(4 * GB, cap, Some(40 * GB)));
        assert!(should_shed(6 * GB, cap, Some(40 * GB)));
        assert!(should_shed(GB, cap, Some(0)));
        assert!(
            !should_shed(256 << 20, cap, Some(0)),
            "a small browser stays"
        );
        assert!(!should_shed(GB, cap, None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_group_measures_by_its_share() {
        // SAFETY: getpgrp has no preconditions.
        let pgid = unsafe { libc::getpgrp() } as u32;
        let pss = memory_of(Some(pgid)).unwrap();
        assert!(pss > 0, "{pss}");
    }

    #[tokio::test]
    async fn node_upload_reserve_rollback_and_old_picker_cancel() {
        use tessera_proto::{BrowserUploadAction as A, BrowserUploadFile};
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::at(tmp.path().join("node"));
        paths.ensure().unwrap();
        let nb = NodeBrowser::new(paths);
        let stop = CancellationToken::new();
        let (tx, mut rx) = queue::channel(8192, stop.clone());
        let own = Arc::new(OwnCalls::default());
        let mut filter = filter();
        filter.choosers.insert("P1".into(), (12, true));
        let mut uploads = crate::browser::upload::Uploads::new(tmp.path().join("uploads"));
        uploads.free_for_test(|_| Ok(crate::disk::RESERVE));
        *nb.0.run.lock() = Some(Run {
            run: 17,
            tx,
            filter,
            own: own.clone(),
            stop,
            asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_use: Arc::new(Mutex::new(Instant::now())),
            downloads: tmp.path().join("downloads"),
            names: HashMap::new(),
            saved: HashMap::new(),
            kept: 0,
            receiving: HashMap::new(),
            pid: None,
            stops: 0,
            uploads,
            upload_finishes: HashMap::new(),
            #[cfg(windows)]
            job: None,
        });
        let cfg = config(|_| {});
        let ctx = Ctx {
            config: &cfg,
            paused: false,
        };
        let send = |id, node, action: A| {
            nb.commands(
                17,
                vec![cmd(
                    id,
                    "Tessera.uploadFiles",
                    json!({"backendNodeId":node,"owner":"viewer:picker","action":action}),
                    Some("P1"),
                )],
                &ctx,
            )
        };
        send(
            1,
            12,
            A::Start {
                files: vec![BrowserUploadFile {
                    name: "small.txt".into(),
                    size: 1,
                }],
            },
        );
        assert!(
            !tmp.path().join("uploads").exists(),
            "node low-space path stages nothing"
        );
        let token = {
            let mut run = nb.0.run.lock();
            let r = run.as_mut().unwrap();
            r.uploads.free_for_test(|_| Ok(u64::MAX));
            r.uploads
                .start(
                    "P1:viewer:picker",
                    &[BrowserUploadFile {
                        name: "small.txt".into(),
                        size: 1,
                    }],
                )
                .unwrap()
        };
        send(
            2,
            12,
            A::Chunk {
                upload: token.clone(),
                index: 0,
                data: "YQ==".into(),
            },
        );
        send(
            3,
            12,
            A::Finish {
                upload: token.clone(),
            },
        );
        let msg = rx.try_recv().unwrap();
        let value: Value = serde_json::from_slice(&msg.value).unwrap();
        let path = value["params"]["files"][0].as_str().unwrap();
        assert!(Path::new(path).exists());
        nb.0.on_message(
            17,
            json!({"id":3,"sessionId":"P1","error":{"message":"browser failed"}})
                .to_string()
                .as_bytes(),
            &own,
        )
        .unwrap();
        assert!(
            !Path::new(path).exists(),
            "failed DOM acceptance rolls node files back"
        );
        let token = {
            let mut run = nb.0.run.lock();
            run.as_mut()
                .unwrap()
                .uploads
                .start(
                    "P1:viewer:picker",
                    &[BrowserUploadFile {
                        name: "cancel.txt".into(),
                        size: 1,
                    }],
                )
                .unwrap()
        };
        nb.0.run
            .lock()
            .as_mut()
            .unwrap()
            .filter
            .choosers
            .remove("P1");
        send(
            4,
            0,
            A::Cancel {
                upload: token.clone(),
            },
        );
        assert!(
            nb.0.run
                .lock()
                .as_mut()
                .unwrap()
                .uploads
                .finish("P1:viewer:picker", &token)
                .is_err()
        );
        assert!(
            std::fs::read_dir(tmp.path().join("uploads"))
                .unwrap()
                .next()
                .is_none()
        );
        nb.stop("test finished");
    }
    #[tokio::test]
    async fn pause_and_targeted_stop_interrupt_a_never_answered_browser_close() {
        for targeted in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let paths = Paths::at(tmp.path().join("node"));
            paths.ensure().unwrap();
            let nb = NodeBrowser::new(paths.clone());
            let stop = CancellationToken::new();
            let (tx, mut rx) = queue::channel(8192, stop.clone());
            let r = Run {
                run: 17,
                tx,
                filter: Filter::new(8),
                own: Arc::new(OwnCalls::default()),
                stop: stop.clone(),
                asked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last_use: Arc::new(Mutex::new(Instant::now())),
                downloads: tmp.path().join("downloads"),
                names: HashMap::new(),
                saved: HashMap::new(),
                kept: 0,
                receiving: HashMap::new(),
                pid: None,
                stops: 0,
                upload_finishes: HashMap::new(),
                uploads: crate::browser::upload::Uploads::new(tmp.path().join("uploads")),
                #[cfg(windows)]
                job: None,
            };
            *nb.0.run.lock() = Some(r);
            let guard = nb.0.profile_gate.clone().lock_owned().await;
            let r = nb.0.take_stopping(17).unwrap();
            let ending = tokio::spawn(r.save_and_stop());
            let close = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let m: Value = serde_json::from_slice(&close.value).unwrap();
            assert_eq!(m["method"], "Browser.close");
            // No reply, and run is already removed. Profile remains reserved.
            assert!(nb.0.run.lock().is_none());
            assert!(nb.0.profile_gate.try_lock().is_err());
            if targeted {
                let cfg = config(|_| {});
                let ctx = Ctx {
                    config: &cfg,
                    paused: false,
                };
                let (ups, _) = tokio::sync::mpsc::unbounded_channel();
                let link: Arc<dyn Link> = Arc::new(TestLink(ups));
                nb.handle(&link, vec![NodeBrowserDown::Stop { run: 18 }], &ctx);
                assert!(!stop.is_cancelled(), "wrong run cannot cancel this exit");
                nb.handle(&link, vec![NodeBrowserDown::Stop { run: 17 }], &ctx);
            } else {
                nb.stop("this machine is paused");
            }
            assert!(stop.is_cancelled());
            tokio::time::timeout(Duration::from_millis(300), ending)
                .await
                .unwrap()
                .unwrap();
            drop(guard);
            assert!(nb.0.profile_gate.try_lock().is_ok());
        }
    }

    #[cfg(any(unix, windows))]
    #[tokio::test(start_paused = true)]
    async fn node_still_closing_admission_is_bounded_and_never_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let nb = NodeBrowser::new(Paths::at(tmp.path()));
        let guard = nb.0.profile_gate.clone().lock_owned().await;
        let stop = CancellationToken::new();
        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = queue::channel(8192, stop.clone());
        let result =
            nb.0.clone()
                .launch(
                    1,
                    Path::new("/must-not-launch"),
                    tmp.path(),
                    None,
                    tx,
                    rx,
                    Arc::new(OwnCalls::default()),
                    stop,
                    asked.clone(),
                )
                .await;
        assert_eq!(
            result.err().unwrap(),
            crate::browser::process::STILL_CLOSING
        );
        assert!(
            asked.load(Ordering::SeqCst),
            "not an unexpected startup crash"
        );
        nb.stop("paused while still closing");
        assert!(nb.0.profile_gate.try_lock().is_err());
        drop(guard);
        assert!(nb.0.profile_gate.try_lock().is_ok());
    }
}
