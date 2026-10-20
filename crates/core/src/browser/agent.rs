//! What the `browser` tool does to a tab: read it as an outline with
//! element refs, find elements, click, type, scroll and wait like a person
//! (real input events, so the people watching see it happen), and take
//! screenshots. Policy (what needs approval) lives in the tool.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::tab::{Link, Tab, WORLD};
use super::{Browser, keys};

const PAGE_JS: &str = include_str!("page.js");
/// Headless agent tabs each session may keep.
pub(super) const MAX_OWN_TABS: usize = 6;

/// An element an action is about to act on.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ElementInfo {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: String,
    pub name: String,
    pub tag: String,
    #[serde(rename = "type")]
    pub input_type: String,
    pub password: bool,
    pub editable: bool,
    pub disabled: bool,
    pub form: Option<FormInfo>,
    pub submit: bool,
    pub sensitive: bool,
    pub href: String,
    pub download: bool,
    pub file: bool,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub left: f64,
    pub top: f64,
    pub covered: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FormInfo {
    pub method: String,
    pub password: bool,
    pub search: bool,
    pub name: String,
}

impl ElementInfo {
    /// `button "Sign in" [e4]`
    pub fn describe(&self) -> String {
        let mut s = self.role.clone();
        if !self.name.is_empty() {
            s.push_str(&format!(" \"{}\"", self.name));
        }
        if !self.reference.is_empty() {
            s.push_str(&format!(" [{}]", self.reference));
        }
        s
    }
}

/// What an action changed on the page (see `page.js` `changes`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Changes {
    /// A new document since (everything shown is new).
    pub fresh: bool,
    /// Elements, text and attributes that changed.
    pub mutations: u64,
    /// The element acted on is drawn by itself (a canvas, video, another
    /// site's frame): what changes inside it can't be seen from the page.
    pub opaque: bool,
    /// Its state before and after ("unchecked" → "checked"; "gone").
    pub state: Option<(String, String)>,
    /// Its state, the same as before.
    pub unchanged: Option<String>,
    /// What has the focus now, if it moved to another element.
    pub focus: Option<String>,
    pub scrolled: bool,
    /// The address now, if it changed within the page.
    pub url: Option<String>,
}

impl Changes {
    /// In words, for the agent: what changed, or that nothing visibly did.
    pub fn describe(&self) -> String {
        if self.fresh {
            return "The page loaded anew.".into();
        }
        let mut parts = Vec::new();
        match (&self.state, &self.unchanged) {
            (Some((_, after)), _) if after == "gone" => {
                parts.push("the element is gone from the page".to_string())
            }
            (Some((before, after)), _) if before.is_empty() => {
                parts.push(format!("it is now {after}"))
            }
            (Some((before, after)), _) => parts.push(format!("it went from {before} to {after}")),
            (None, Some(same)) => parts.push(format!("it is still {same}")),
            _ => {}
        }
        if let Some(f) = &self.focus {
            parts.push(format!("the focus moved to {f}"));
        }
        if self.url.is_some() {
            parts.push("the address changed".into());
        }
        if self.mutations > 0 {
            parts.push(format!(
                "the page changed ({} update{})",
                self.mutations,
                if self.mutations == 1 { "" } else { "s" }
            ));
        }
        if self.scrolled {
            parts.push("the page scrolled".into());
        }
        let changed = self.state.is_some()
            || self.focus.is_some()
            || self.url.is_some()
            || self.mutations > 0
            || self.scrolled;
        if !changed && self.opaque {
            return format!(
                "{}What it draws itself (a canvas, video or another site's frame) can't be read from the page: take a `screenshot` to see whether it reacted.",
                parts
                    .first()
                    .map(|p| {
                        let mut c = p.chars();
                        let first: String =
                            c.next().into_iter().flat_map(char::to_uppercase).collect();
                        format!("{first}{}. ", c.as_str())
                    })
                    .unwrap_or_default()
            );
        }
        if !changed {
            let still = parts.first().map(|p| format!(" ({p})")).unwrap_or_default();
            return format!(
                "Nothing visibly changed{still}: no navigation, and no change to the page, its focus or the element's state. The click may have missed or the page may ignore it; check with a `screenshot` or `snapshot` before clicking again."
            );
        }
        let s = parts.join("; ");
        if self.opaque {
            return format!(
                "After it, {s}. What it draws itself can't be read from the page: take a `screenshot` to see whether it reacted."
            );
        }
        format!("After it, {s}.")
    }
}

/// A page outline for the agent.
pub struct Snapshot {
    pub title: String,
    pub url: String,
    pub text: String,
    /// Lines left out to stay within the size asked for.
    pub dropped: u64,
    pub viewport: (u32, u32),
    pub scroll: (i64, i64),
}

/// A picture of the tab.
pub struct Shot {
    pub image: tessera_proto::SnapshotImage,
    /// Image pixels per CSS pixel.
    pub scale: f64,
}

/// A tab as the agent's `tabs` list shows it.
pub struct AgentTab {
    pub key: String,
    pub board: Option<String>,
    /// The machine it runs on (none: this server).
    pub machine: Option<String>,
    pub title: String,
    pub url: String,
    pub watched: bool,
    pub live: bool,
    /// The address of a popup open on top of it.
    pub popup: Option<String>,
}

impl Browser {
    /// This session's agent's own headless tabs here.
    pub(crate) fn own_tabs(&self, session: &str) -> Vec<Arc<Tab>> {
        let mut v: Vec<Arc<Tab>> = self
            .0
            .tabs
            .lock()
            .values()
            .filter(|t| t.owner().as_deref() == Some(session))
            .cloned()
            .collect();
        v.sort_by_key(|t| t.key());
        v
    }

    /// Make a bounded headless handle. At the chat cap, explicit closure
    /// is safer than retiring another request's tab during page creation.
    pub(crate) fn new_agent_tab(&self, session: &str, url: &str) -> Result<Arc<Tab>, String> {
        let mut tabs = self.0.tabs.lock();
        let mut own: Vec<_> = tabs
            .values()
            .filter(|t| t.owner().as_deref() == Some(session))
            .map(|t| t.key())
            .collect();
        if own.len() >= MAX_OWN_TABS {
            own.sort();
            return Err(format!(
                "This chat has {MAX_OWN_TABS} tabs. Close one of {} before opening another.",
                own.iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let key = format!("t{}", self.0.hub.next_id());
        let tab = Tab::new(
            &self.0,
            key.clone(),
            None,
            Some(session.to_string()),
            None,
            url.to_string(),
        );
        tabs.insert(key, tab.clone());
        Ok(tab)
    }

    /// Put one of an agent's headless tabs on a board (its tile), keeping
    /// the page as it is.
    pub(crate) fn adopt(&self, tab: &Arc<Tab>, board: &str, chat: &str) {
        let mut tabs = self.0.tabs.lock();
        tabs.remove(&tab.key());
        // A tab the board may already have (a device attached to the new
        // tile first) is replaced by this one, and its devices move over.
        let mut moved = Vec::new();
        if let Some(old) = tabs.remove(board) {
            drop(tabs);
            moved = old.viewers();
            old.close();
            tabs = self.0.tabs.lock();
        }
        for (id, view, takes) in moved {
            tab.attach(id, view, takes);
        }
        tab.rekey(board, chat);
        tabs.insert(board.to_string(), tab.clone());
    }

    /// Close one of an agent's headless tabs here.
    pub(crate) fn close_own_tab(&self, session: &str, key: &str) -> bool {
        let tab = {
            let mut tabs = self.0.tabs.lock();
            match tabs.get(key) {
                Some(t) if t.owner().as_deref() == Some(session) => tabs.remove(key),
                _ => None,
            }
        };
        match tab {
            Some(t) => {
                t.close();
                self.note_closed(
                    key,
                    session,
                    format!("tab `{key}` was closed; open it again if you still need it"),
                );
                true
            }
            None => false,
        }
    }
}

pub(crate) fn describe(t: &Arc<Tab>) -> AgentTab {
    AgentTab {
        key: t.key(),
        board: t.board(),
        machine: t.machine(),
        title: t.title(),
        url: t.url(),
        watched: t.watched(),
        live: t.is_live(),
        popup: Some(t.top()).filter(|p| p.is_popup()).map(|p| p.url()),
    }
}

/// Marks a tab as being worked on by an agent while it lives.
pub struct Acting<'a> {
    tab: &'a Tab,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl Drop for Acting<'_> {
    fn drop(&mut self) {
        self.tab.with_state(|st| st.agent_since = None);
    }
}

impl Tab {
    /// Hold the tab for one agent action (others wait their turn).
    pub async fn acting(&self) -> Acting<'_> {
        let guard = self.act.lock().await;
        self.with_state(|st| st.agent_since = Some(Instant::now()));
        let acting = Acting {
            tab: self,
            _guard: guard,
        };
        self.wake().await;
        acting
    }

    /// Evaluate `expr` in Tessera's isolated world (with `__tsr` loaded
    /// when `page` is set).
    pub(crate) async fn eval_in(
        &self,
        link: &Link,
        expr: &str,
        page: bool,
    ) -> Result<Value, String> {
        if let Some((kind, message, _)) = self.dialog() {
            return Err(format!(
                "a {} dialog is open (\"{}\"): answer it with action `dialog` first",
                format!("{kind:?}").to_lowercase(),
                crate::util::one_line(&message, 200)
            ));
        }
        let s = Some(link.session.as_str());
        let ctx = link
            .cdp
            .call(
                s,
                "Page.createIsolatedWorld",
                json!({"frameId": link.target, "worldName": WORLD, "grantUniveralAccess": false}),
            )
            .await?["executionContextId"]
            .clone();
        let expression = if page {
            format!("{PAGE_JS}\n;{expr}")
        } else {
            expr.to_string()
        };
        let r = link
            .cdp
            .call_timeout(
                s,
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": ctx,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
                Duration::from_secs(40),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex["exception"]["description"]
                .as_str()
                .or_else(|| ex["text"].as_str())
                .unwrap_or("the page script failed");
            let first = msg.lines().next().unwrap_or(msg);
            return Err(first
                .trim_start_matches("Uncaught ")
                .trim_start_matches("Error: ")
                .to_string());
        }
        Ok(r["result"]["value"].clone())
    }

    async fn page(&self, expr: &str) -> Result<Value, String> {
        let link = self.link()?;
        self.eval_in(&link, expr, true).await
    }

    pub async fn snapshot(&self, filter: Option<&str>, max: usize) -> Result<Snapshot, String> {
        let v = self
            .page(&format!(
                "__tsr.snapshot({})",
                json!({"filter": filter.unwrap_or(""), "max": max})
            ))
            .await?;
        let h = &v["head"];
        Ok(Snapshot {
            title: h["title"].as_str().unwrap_or_default().to_string(),
            url: h["url"].as_str().unwrap_or_default().to_string(),
            text: v["text"].as_str().unwrap_or_default().to_string(),
            dropped: h["dropped"].as_u64().unwrap_or(0),
            viewport: (
                h["vw"].as_u64().unwrap_or(0) as u32,
                h["vh"].as_u64().unwrap_or(0) as u32,
            ),
            scroll: (h["sy"].as_i64().unwrap_or(0), h["sh"].as_i64().unwrap_or(0)),
        })
    }

    /// The element `target` names (`{"ref"}`, `{"text"}` or `{"x","y"}` in
    /// CSS pixels), scrolled into view when `aim`, with where to click it.
    pub async fn probe(&self, target: &Value, aim: bool) -> Result<ElementInfo, String> {
        let v = self.page(&format!("__tsr.probe({target}, {aim})")).await?;
        serde_json::from_value(v).map_err(|e| format!("unexpected answer from the page: {e}"))
    }

    /// Start noting what the next action changes on the page (`target`: the
    /// element it acts on). False when the page can't be watched.
    pub async fn watch_changes(&self, target: &Value) -> bool {
        self.page(&format!("__tsr.watch({target})"))
            .await
            .is_ok_and(|v| v == json!(true))
    }

    /// What changed on the page since [`Tab::watch_changes`] (`None`: it
    /// can't be read now, e.g. a dialog is open).
    pub async fn page_changes(&self) -> Option<Changes> {
        let v = self.page("__tsr.changes()").await.ok()?;
        serde_json::from_value(v).ok()
    }

    /// Outline what is about to be clicked, for the people watching.
    pub async fn flash(&self, e: &ElementInfo) {
        if self.viewer_count() == 0 || e.w <= 0.0 {
            return;
        }
        let _ = self
            .page(&format!(
                "__tsr.flash({}, {}, {}, {})",
                e.left, e.top, e.w, e.h
            ))
            .await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    /// A real click (move, press, release) at CSS pixel `x`,`y`.
    pub async fn click_at(&self, x: f64, y: f64, clicks: u32) -> Result<(), String> {
        let link = self.link()?;
        let s = Some(link.session.as_str());
        let base = json!({"x": x, "y": y, "pointerType": "mouse"});
        let mut ev = base.clone();
        ev["type"] = json!("mouseMoved");
        link.cdp.call(s, "Input.dispatchMouseEvent", ev).await?;
        for n in 1..=clicks.max(1) {
            for (ty, buttons) in [("mousePressed", 1), ("mouseReleased", 0)] {
                let mut ev = base.clone();
                ev["type"] = json!(ty);
                ev["button"] = json!("left");
                ev["buttons"] = json!(buttons);
                ev["clickCount"] = json!(n);
                // A dialog the click opens blocks the release's reply.
                link.cdp
                    .call_timeout(s, "Input.dispatchMouseEvent", ev, Duration::from_secs(5))
                    .await
                    .or_else(|e| {
                        if self.dialog().is_some() {
                            Ok(Value::Null)
                        } else {
                            Err(e)
                        }
                    })?;
            }
        }
        Ok(())
    }

    /// Focus a text field and select what it holds (typing replaces it).
    pub async fn focus_field(&self, target: &Value) -> Result<ElementInfo, String> {
        let v = self.page(&format!("__tsr.focus({target})")).await?;
        serde_json::from_value(v).map_err(|e| format!("unexpected answer from the page: {e}"))
    }

    /// Type text at the caret as real input (IME-style insertion).
    pub async fn insert_text(&self, text: &str) -> Result<(), String> {
        let link = self.link()?;
        link.cdp
            .call(
                Some(&link.session),
                "Input.insertText",
                json!({"text": text}),
            )
            .await
            .map(|_| ())
    }

    /// Press a key or combination (`Enter`, `Control+a`, `Shift+Tab`).
    pub async fn press(&self, combo: &str) -> Result<(), String> {
        let (key, code, mods) = keys::parse_combo(combo)?;
        let link = self.link()?;
        let s = Some(link.session.as_str());
        // Modifiers go down first and come up last, as on a keyboard.
        let held: Vec<(&str, &str, u32)> = [
            (keys::CTRL, "Control", "ControlLeft"),
            (keys::SHIFT, "Shift", "ShiftLeft"),
            (keys::ALT, "Alt", "AltLeft"),
            (keys::META, "Meta", "MetaLeft"),
        ]
        .into_iter()
        .filter(|(bit, ..)| mods & bit != 0)
        .map(|(bit, k, c)| (k, c, bit))
        .collect();
        let mut down = 0;
        for (k, c, bit) in &held {
            down |= bit;
            link.cdp
                .call(
                    s,
                    "Input.dispatchKeyEvent",
                    keys::event(true, k, c, None, down),
                )
                .await?;
        }
        let text = keys::implied_text(&key, mods);
        let r = link
            .cdp
            .call_timeout(
                s,
                "Input.dispatchKeyEvent",
                keys::event(true, &key, &code, text.as_deref(), mods),
                Duration::from_secs(5),
            )
            .await;
        if r.is_err() && self.dialog().is_none() {
            r?;
        }
        let _ = link
            .cdp
            .call_timeout(
                s,
                "Input.dispatchKeyEvent",
                keys::event(false, &key, &code, None, mods),
                Duration::from_secs(5),
            )
            .await;
        for (k, c, bit) in held.iter().rev() {
            down &= !bit;
            let _ = link
                .cdp
                .call_timeout(
                    s,
                    "Input.dispatchKeyEvent",
                    keys::event(false, k, c, None, down),
                    Duration::from_secs(5),
                )
                .await;
        }
        Ok(())
    }

    pub async fn select(&self, target: &Value, values: &[String]) -> Result<Vec<String>, String> {
        let v = self
            .page(&format!("__tsr.select({target}, {})", json!(values)))
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Bring an element into view.
    pub async fn reveal(&self, target: &Value) -> Result<ElementInfo, String> {
        let v = self.page(&format!("__tsr.reveal({target})")).await?;
        serde_json::from_value(v).map_err(|e| format!("unexpected answer from the page: {e}"))
    }

    /// Scroll with the mouse wheel at `x`,`y` (so the panel under it scrolls).
    pub async fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) -> Result<(), String> {
        let link = self.link()?;
        link.cdp
            .call(
                Some(&link.session),
                "Input.dispatchMouseEvent",
                json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "pointerType": "mouse"}),
            )
            .await?;
        // Smooth scrolling takes a moment.
        tokio::time::sleep(Duration::from_millis(350)).await;
        Ok(())
    }

    /// Wait until `text` shows on the page (or is gone), up to `ms`.
    pub async fn wait_text(&self, text: &str, ms: u64, gone: bool) -> Result<bool, String> {
        let v = self
            .page(&format!("__tsr.waitText({}, {ms}, {gone})", json!(text)))
            .await?;
        Ok(v == json!(true))
    }

    /// The focused element, if any.
    pub async fn focused(&self) -> Option<ElementInfo> {
        let v = self.page("__tsr.focused()").await.ok()?;
        serde_json::from_value(v).ok()
    }

    /// Bring the title and address up to date (title changes aren't
    /// reported while no tile watches).
    pub async fn refresh(&self) {
        let Ok(link) = self.link() else { return };
        if let Ok(i) = link
            .cdp
            .call(
                None,
                "Target.getTargetInfo",
                json!({"targetId": link.target}),
            )
            .await
        {
            let info = &i["targetInfo"];
            self.on_info(
                info["title"].as_str().unwrap_or_default(),
                info["url"].as_str().unwrap_or_default(),
            );
        }
    }

    /// Wait until the page stops loading (or `max` passes); true if it did.
    pub async fn wait_load(&self, max: Duration) -> bool {
        let mut rx = self.changes();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            if !self.loading() || self.dialog().is_some() || !self.is_live() {
                return true;
            }
            if tokio::time::timeout_at(deadline, rx.changed())
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    /// After an action: give the page a moment to react, and wait for a
    /// navigation it started.
    pub async fn settle(&self) {
        let mut rx = self.changes();
        let quick = tokio::time::Instant::now() + Duration::from_millis(350);
        while !self.loading() {
            if tokio::time::timeout_at(quick, rx.changed()).await.is_err() {
                break;
            }
        }
        if self.loading() {
            self.wait_load(Duration::from_secs(15)).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// A JPEG of what the tab shows, at most `max_side` pixels long.
    pub async fn screenshot(&self, max_side: u32, quality: u8) -> Result<Shot, String> {
        let link = self.link()?;
        let s = Some(link.session.as_str());
        let m = link.cdp.call(s, "Page.getLayoutMetrics", json!({})).await?;
        let vv = &m["cssVisualViewport"];
        let w = vv["clientWidth"].as_f64().unwrap_or(1280.0).max(1.0);
        let h = vv["clientHeight"].as_f64().unwrap_or(800.0).max(1.0);
        // The emulated device pixel ratio (see `Tab::apply_view`).
        let (view, zoom) = self.view();
        let dsf = view.page_dpr(self.scale()) * zoom;
        // Image pixels per CSS pixel: the device's, capped by `max_side`.
        let scale = (max_side as f64 / w.max(h)).min(dsf);
        let r = link
            .cdp
            .call(
                s,
                "Page.captureScreenshot",
                json!({
                    "format": "jpeg",
                    "quality": quality,
                    "clip": {
                        "x": vv["pageX"].as_f64().unwrap_or(0.0),
                        "y": vv["pageY"].as_f64().unwrap_or(0.0),
                        "width": w,
                        "height": h,
                        "scale": scale / dsf,
                    },
                    "captureBeyondViewport": false,
                }),
            )
            .await?;
        let data = r["data"].as_str().unwrap_or_default().to_string();
        if data.is_empty() {
            return Err("the browser returned no picture".into());
        }
        Ok(Shot {
            image: tessera_proto::SnapshotImage {
                mime: "image/jpeg".into(),
                data,
                width: (w * scale).round() as u32,
                height: (h * scale).round() as u32,
            },
            scale,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_says_what_changed_or_that_nothing_did() {
        let c = |v: Value| -> Changes { serde_json::from_value(v).unwrap() };
        assert_eq!(
            c(json!({"state": ["unchecked", "checked"]})).describe(),
            "After it, it went from unchecked to checked."
        );
        assert_eq!(
            c(json!({"unchanged": "unchecked"})).describe(),
            "Nothing visibly changed (it is still unchecked): no navigation, and no change to the page, its focus or the element's state. The click may have missed or the page may ignore it; check with a `screenshot` or `snapshot` before clicking again."
        );
        assert!(
            c(json!({"mutations": 3, "focus": "textbox \"Email\" [e9]"}))
                .describe()
                .contains(
                    "the focus moved to textbox \"Email\" [e9]; the page changed (3 updates)"
                )
        );
        assert!(
            c(json!({"opaque": true}))
                .describe()
                .starts_with("What it draws itself")
        );
        assert!(
            c(json!({"opaque": true, "unchanged": "pressed: false"}))
                .describe()
                .starts_with("It is still pressed: false. What it draws")
        );
        assert_eq!(
            c(json!({"fresh": true})).describe(),
            "The page loaded anew."
        );
        assert!(
            c(json!({"state": ["", "gone"]}))
                .describe()
                .contains("the element is gone")
        );
    }
}
