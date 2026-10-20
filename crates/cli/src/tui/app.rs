//! The terminal app's state and behaviour: one chat across the whole
//! width (or a few side by side, `/split`), backend events, keys and slash
//! commands. Every pane owns its chat's state ([`Pane`]); the app owns the
//! panes and which one has the keyboard.
//!
//! Terminal chats are a lean coding harness (`SessionInfo::lean`): no
//! tiles of other kinds, no bots, notes or web views. Chats stay the
//! server's, so the apps show them and the terminal opens theirs.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use serde_json::Value;
use tessera_client::ConnState;
use tessera_proto::*;
use tokio::sync::mpsc;

use super::commands;
use super::motif::{self, Motion};
use super::pane::{CompItem, CompKind, Completion, Effect, Pane, PaneId};
use super::text::{self, span};
use super::theme::{Depth, Theme, ThemePref};
use super::transcript::BotLook;
use crate::backend::{Backend, BackendEvent};

pub type Job = Box<dyn FnOnce(&mut App) + Send>;

/// The narrowest a chat beside another may be.
pub const MIN_PANE_W: u16 = 44;
/// At most this many chats side by side.
pub const MAX_PANES: usize = 4;
/// How long an approval card shows before keys answer it.
const APPROVAL_GUARD: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickKind {
    Chats,
    Model,
    /// The model new chats start with (Settings).
    DefaultModel,
    Thinking,
    Permission,
    Theme,
    Rewind,
    Settings,
}

/// A mouse target drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A chat (a click gives it the keyboard).
    Pane(PaneId),
}

/// Where the open popup's items were drawn (for the mouse).
#[derive(Default, Clone)]
pub struct ModalHits {
    pub popup: Rect,
    /// (row, item index).
    pub items: Vec<(Rect, usize)>,
}

impl ModalHits {
    pub fn item(&self, x: u16, y: u16) -> Option<usize> {
        let p = ratatui::layout::Position { x, y };
        self.items.iter().find(|(r, _)| r.contains(p)).map(|h| h.1)
    }
}

/// Something to confirm before doing it.
pub struct Confirm {
    pub title: String,
    pub text: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// YOLO for this chat and new ones (explained first).
    Permission(PermissionMode),
}

pub struct PickItem {
    pub label: String,
    pub detail: String,
    pub group: String,
    pub value: String,
    pub current: bool,
    pub badge: String,
    pub disabled: bool,
}

impl PickItem {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            group: String::new(),
            value: value.into(),
            current: false,
            badge: String::new(),
            disabled: false,
        }
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }
}

pub struct Picker {
    pub kind: PickKind,
    pub title: String,
    pub items: Vec<PickItem>,
    pub filter: String,
    pub selected: usize,
    pub loading: bool,
    /// Shown when there is nothing to list.
    pub empty: String,
    /// Opened from the settings: closing it goes back there.
    pub from_settings: bool,
}

impl Picker {
    pub fn new(kind: PickKind, title: &str) -> Self {
        Self {
            kind,
            title: title.into(),
            items: Vec::new(),
            filter: String::new(),
            selected: 0,
            loading: true,
            empty: String::new(),
            from_settings: false,
        }
    }

    /// Indices of items matching the filter, best first.
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.trim().is_empty() {
            return (0..self.items.len()).collect();
        }
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let hay = format!("{} {} {}", it.group, it.label, it.detail);
                let s = commands::fuzzy(&self.filter, &it.label)
                    .map(|s| s + 50)
                    .or_else(|| commands::fuzzy(&self.filter, &hay));
                s.map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    pub(super) fn select_current(&mut self) {
        let vis = self.visible();
        if let Some(p) = vis.iter().position(|i| self.items[*i].current) {
            self.selected = p;
        }
    }

    /// Lists you type into (filter); the short ones take keys instead.
    pub fn filterable(&self) -> bool {
        matches!(
            self.kind,
            PickKind::Chats | PickKind::Model | PickKind::DefaultModel | PickKind::Rewind
        )
    }
}

pub struct Info {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: usize,
}

pub enum Modal {
    Picker(Picker),
    Info(Info),
    Confirm(Confirm),
    Rewind(Box<RewindForm>),
}

/// `/rewind`: the dry run's preview and the choices before confirming.
pub struct RewindForm {
    pub session_id: String,
    /// Your message being rewound to (it and everything after goes).
    pub seq: u64,
    pub text: String,
    pub result: RewindResult,
    pub files: bool,
    pub conversation: bool,
    pub running: bool,
}

impl RewindForm {
    pub fn file_changes(&self) -> usize {
        self.result.restored.len() + self.result.deleted.len()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Boot {
    Starting,
    WaitingForModel,
    NoModel,
    Ready,
}

pub struct Toast {
    pub text: String,
    pub style: Style,
    pub until: Instant,
}

/// How to start a chat in a pane.
#[derive(Clone, Default)]
pub struct NewChat {
    pub cwd: Option<String>,
    pub model: Option<ModelRef>,
}

/// What to do when a chat cannot be opened in a pane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OnFail {
    /// Keep what the pane shows and report the error.
    Report,
    /// Start a new chat there instead.
    NewChat,
    /// Start a new chat there without a word (the chat last open is gone).
    NewQuietly,
}

pub struct App {
    pub backend: Backend,
    pub theme: Theme,
    pub(super) tx: mpsc::UnboundedSender<Job>,
    pub started: Instant,

    /// The chats on screen, left to right.
    pub panes: Vec<Pane>,
    pub focus: PaneId,
    pub(super) next_pane: PaneId,
    /// Last drawn frame size.
    pub screen: Rect,
    /// Mouse targets of the last frame.
    pub hits: Vec<(Rect, Hit)>,
    /// Mouse targets of the open popup.
    pub modal_hits: ModalHits,
    /// Whiteboards of the chats on screen, by id: their questions are
    /// answered here; everything else they show is the apps'.
    pub boards: HashMap<String, Whiteboard>,
    /// How much the interface animates (reduced on slow terminals).
    pub motion: Motion,
    /// Recent frame times (ms, smoothed): how fast this terminal draws.
    frame_ms: f32,
    /// Frames measured so far (the first ones repaint everything).
    frames: u32,
    /// The theme to go back to while the theme list previews another.
    theme_saved: Option<ThemePref>,

    // sessions
    pub(super) created_here: HashSet<String>,
    pub boot: Boot,
    boot_since: Instant,
    want_model: Option<ModelRef>,
    pub(super) version: u64,
    /// The user chose a model (flag or /model); don't second-guess it.
    model_explicit: bool,
    /// `--yolo`: new chats start in YOLO mode.
    pub yolo_default: bool,
    /// How much freedom new chats get (Settings); `None`: the server's.
    pub new_permission: Option<PermissionMode>,
    /// Chats already told they have tools the terminal doesn't show.
    told_app_tools: HashSet<String>,
    /// The settings row last used (the list comes back to it).
    pub(super) settings_row: usize,

    // server
    pub server: Option<ServerInfo>,
    pub providers: Vec<ProviderInfo>,
    pub conn: ConnState,
    /// Bots by id, for naming who speaks in a chat from the apps.
    pub bot_looks: HashMap<String, BotLook>,

    // ui
    pub(super) proc_history: Vec<String>,
    dir_cache: HashMap<PathBuf, (Instant, Vec<(String, bool)>)>,
    dir_pending: HashSet<PathBuf>,
    pub modal: Option<Modal>,
    pub expand: bool,
    pub show_thinking: bool,
    pub plan_collapsed: bool,
    pub mouse: bool,
    pub mouse_changed: bool,
    pub prefs_file: Option<PathBuf>,
    pub toast: Option<Toast>,
    last_ctrl_c: Option<Instant>,
    pub quit: bool,
    pub suspend: bool,
    pub force_clear: bool,
    pub dirty: bool,
    pub kbd_enhanced: bool,
}

const THINKING: [ThinkingLevel; 8] = [
    ThinkingLevel::Auto,
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::XHigh,
    ThinkingLevel::Max,
];

/// The permission presets, as in the apps: (mode, label, what it means).
pub const PERMS: [(PermissionMode, &str, &str); 4] = [
    (
        PermissionMode::Default,
        "Default",
        "Works on its own; asks before deleting things or risky commands",
    ),
    (
        PermissionMode::Ask,
        "Ask me",
        "Checks with you before every edit or command",
    ),
    (
        PermissionMode::Auto,
        "YOLO",
        "Never asks; catastrophic commands are still blocked",
    ),
    (
        PermissionMode::ReadOnly,
        "Read-only",
        "Reads and plans; changes nothing",
    ),
];

pub fn thinking_name(t: ThinkingLevel) -> &'static str {
    match t {
        ThinkingLevel::Auto => "auto",
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// The preset's name (`Default`, `Ask me`, `YOLO`, `Read-only`).
pub fn perm_label(p: PermissionMode) -> &'static str {
    PERMS.iter().find(|x| x.0 == p).map_or("Default", |x| x.1)
}

pub fn perm_desc(p: PermissionMode) -> &'static str {
    PERMS.iter().find(|x| x.0 == p).map_or("", |x| x.2)
}

/// The name kept in the preferences file.
pub fn permission_name(p: PermissionMode) -> &'static str {
    match p {
        PermissionMode::Default => "default",
        PermissionMode::Auto => "yolo",
        PermissionMode::Ask => "ask",
        PermissionMode::ReadOnly => "readonly",
    }
}

pub(super) fn parse_thinking(s: &str) -> Option<ThinkingLevel> {
    THINKING
        .into_iter()
        .find(|t| thinking_name(*t) == s.trim().to_ascii_lowercase())
}

pub fn parse_permission(s: &str) -> Option<PermissionMode> {
    let s = s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
    match s.as_str() {
        "auto" | "yolo" => Some(PermissionMode::Auto),
        "default" => Some(PermissionMode::Default),
        "ask" | "askme" => Some(PermissionMode::Ask),
        "readonly" => Some(PermissionMode::ReadOnly),
        _ => None,
    }
}

#[derive(Default)]
struct Upd {
    model: Option<ModelRef>,
    thinking: Option<ThinkingLevel>,
    permission: Option<PermissionMode>,
}

pub fn short_path(p: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let h = home.display().to_string();
        if p == h {
            return "~".into();
        }
        if let Some(rest) = p.strip_prefix(&format!("{h}/")) {
            return format!("~/{rest}");
        }
    }
    p.to_string()
}

/// `short_path`, keeping only the tail when longer than `max` cells.
pub fn compact_path(p: &str, max: usize) -> String {
    let s = short_path(p);
    if text::width(&s) <= max {
        return s;
    }
    let parts: Vec<&str> = s.split('/').collect();
    let mut out = String::new();
    for part in parts.iter().rev() {
        let candidate = if out.is_empty() {
            part.to_string()
        } else {
            format!("{part}/{out}")
        };
        if text::width(&candidate) + 2 > max {
            break;
        }
        out = candidate;
    }
    if out.is_empty() {
        text::truncate(&s, max)
    } else {
        format!("…/{out}")
    }
}

pub fn ago(ms: u64) -> String {
    let now = tessera_core::util::now_ms();
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// A chat `/chats` lists and the terminal reopens: yours, not a bot's or a
/// room, not private, not a helper's.
pub fn listed(s: &SessionInfo) -> bool {
    s.kind == SessionKind::Chat && s.bot_id.is_none() && !s.private && !s.internal && !s.archived
}

/// The chat to reopen in `cwd`: the most recent one there with anything
/// in it.
pub fn recent_chat(list: &[SessionInfo], cwd: &str) -> Option<String> {
    let canon = tessera_core::platform::canonicalize(Path::new(cwd))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| cwd.to_string());
    list.iter()
        .filter(|s| listed(s) && s.last_seq > 0 && (s.cwd == cwd || s.cwd == canon))
        .max_by_key(|s| s.updated_at)
        .map(|s| s.id.clone())
}

impl App {
    pub fn new(
        backend: Backend,
        theme: Theme,
        plain: bool,
        tx: mpsc::UnboundedSender<Job>,
    ) -> Self {
        let conn = if backend.connected() {
            ConnState::Connected
        } else {
            ConnState::Connecting
        };
        let screen = crossterm::terminal::size()
            .map(|(w, h)| Rect::new(0, 0, w, h))
            .ok()
            .filter(|r| r.width > 0 && r.height > 0)
            .unwrap_or(Rect::new(0, 0, 120, 40));
        Self {
            backend,
            theme,
            tx,
            started: Instant::now(),
            panes: vec![Pane::new(1)],
            focus: 1,
            next_pane: 2,
            screen,
            hits: Vec::new(),
            modal_hits: ModalHits::default(),
            boards: HashMap::new(),
            motion: if plain || !theme.has_color() || std::env::var_os("TESSERA_NO_ANIM").is_some()
            {
                Motion::Off
            } else {
                Motion::Full
            },
            frame_ms: 0.0,
            frames: 0,
            theme_saved: None,
            created_here: HashSet::new(),
            boot: Boot::Starting,
            boot_since: Instant::now(),
            want_model: None,
            version: 0,
            model_explicit: false,
            yolo_default: false,
            new_permission: None,
            told_app_tools: HashSet::new(),
            settings_row: 0,
            server: None,
            providers: Vec::new(),
            conn,
            bot_looks: HashMap::new(),
            proc_history: Vec::new(),
            dir_cache: HashMap::new(),
            dir_pending: HashSet::new(),
            modal: None,
            expand: false,
            show_thinking: false,
            plan_collapsed: false,
            mouse: true,
            mouse_changed: false,
            prefs_file: None,
            toast: None,
            last_ctrl_c: None,
            quit: false,
            suspend: false,
            force_clear: false,
            dirty: true,
            kbd_enhanced: false,
        }
    }

    // ---- async plumbing ----------------------------------------------------------

    pub(super) fn call<T, F>(&self, req: Request, done: F)
    where
        T: serde::de::DeserializeOwned + Send + 'static,
        F: FnOnce(&mut App, Result<T, ApiError>) + Send + 'static,
    {
        let backend = self.backend.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = backend.call_as::<T>(req).await;
            let _ = tx.send(Box::new(move |app: &mut App| {
                done(app, r);
                app.dirty = true;
            }));
        });
    }

    pub(super) fn fire(&self, req: Request) {
        let backend = self.backend.clone();
        tokio::spawn(async move {
            let _ = backend.call(req).await;
        });
    }

    // ---- panes -------------------------------------------------------------------

    pub fn pane(&self) -> &Pane {
        self.panes
            .iter()
            .find(|p| p.id == self.focus)
            .unwrap_or(&self.panes[0])
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        let f = self.focus;
        let i = self.panes.iter().position(|p| p.id == f).unwrap_or(0);
        &mut self.panes[i]
    }

    pub fn pane_by(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_by_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.pane_mut()
            .transcript
            .add_local(NoticeLevel::Error, msg);
        self.dirty = true;
    }

    fn pane_error(&mut self, pid: PaneId, msg: impl Into<String>) {
        match self.pane_by_mut(pid) {
            Some(p) => p.transcript.add_local(NoticeLevel::Error, msg),
            None => self.toast_warn(msg),
        }
        self.dirty = true;
    }

    pub fn set_show_thinking(&mut self, on: bool) {
        self.show_thinking = on;
        self.force_clear = true;
        self.dirty = true;
        self.save_prefs();
        self.toast(if on {
            "Showing the model's thinking (ctrl+t hides it)"
        } else {
            "Hiding the model's thinking (ctrl+t shows it)"
        });
    }

    /// Remember the interface preferences.
    pub(super) fn save_prefs(&mut self) {
        if let Some(f) = &self.prefs_file {
            let mut p = Prefs::load(f);
            p.mouse = self.mouse;
            p.show_thinking = self.show_thinking;
            p.permission = self.new_permission.map(|m| permission_name(m).to_string());
            p.save(f);
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some(Toast {
            text: msg.into(),
            style: self.theme.accent_text(),
            until: Instant::now() + Duration::from_secs(4),
        });
        self.dirty = true;
    }

    pub(super) fn toast_warn(&mut self, msg: impl Into<String>) {
        self.toast = Some(Toast {
            text: msg.into(),
            style: self.theme.warning(),
            until: Instant::now() + Duration::from_secs(5),
        });
        self.dirty = true;
    }

    pub fn session_id(&self) -> Option<String> {
        self.pane().session_id().map(str::to_string)
    }

    pub fn animating(&self) -> bool {
        if self.motion == Motion::Off {
            return false;
        }
        self.panes.iter().any(Pane::animating)
            || matches!(self.boot, Boot::Starting | Boot::WaitingForModel)
            || self.conn != ConnState::Connected
    }

    /// How long one step of the working indicators lasts.
    fn step_ms(&self) -> u64 {
        match self.motion {
            Motion::Full => motif::STEP_MS,
            _ => motif::STEP_MS * 2,
        }
    }

    /// The working indicators' step (constant when motion is off).
    pub fn anim_step(&self) -> usize {
        match self.motion {
            Motion::Off => 2,
            _ => (self.started.elapsed().as_millis() as u64 / self.step_ms()) as usize,
        }
    }

    /// A frame took `d` to draw and write: slow terminals (a slow link,
    /// a busy machine) get less motion so typing stays responsive.
    pub fn note_frame(&mut self, d: Duration) {
        let ms = d.as_secs_f32() * 1000.0;
        self.frames = self.frames.saturating_add(1);
        // Full repaints (the first frames, after a resize or clear) are
        // slow everywhere: judge the terminal by the frames after them.
        if self.frames <= 4 {
            return;
        }
        self.frame_ms = if self.frame_ms == 0.0 {
            ms
        } else {
            self.frame_ms * 0.8 + ms * 0.2
        };
        let next = match self.motion {
            Motion::Full if self.frame_ms > 25.0 => Motion::Reduced,
            Motion::Reduced if self.frame_ms > 70.0 => Motion::Off,
            m => m,
        };
        if next != self.motion {
            self.motion = next;
            self.dirty = true;
        }
    }

    /// Switch theme and remember it.
    pub fn set_theme(&mut self, pref: ThemePref) {
        self.theme_saved = None;
        self.show_theme(pref);
        if let Some(f) = &self.prefs_file {
            let mut p = Prefs::load(f);
            p.theme = Some(pref.name().to_string());
            p.save(f);
        }
        if !self.theme.has_color() {
            self.toast("Colours are off (--plain or NO_COLOR)");
        } else {
            self.toast(format!("Theme: {}", pref.label()));
        }
    }

    /// Repaint everything in another theme (not remembered).
    fn show_theme(&mut self, pref: ThemePref) {
        if pref == self.theme.pref() {
            return;
        }
        self.theme = self.theme.with_pref(pref);
        let t = self.theme;
        for b in self.bot_looks.values_mut() {
            b.style = t.bot(&b.color, &b.name);
        }
        self.force_clear = true;
        self.dirty = true;
    }

    /// While the theme list is open, everything shows in the highlighted
    /// theme; closing it without choosing puts the previous one back.
    pub(super) fn preview_theme(&mut self) {
        let picking = match &self.modal {
            Some(Modal::Picker(p)) if p.kind == PickKind::Theme => Some(
                p.visible()
                    .get(p.selected)
                    .and_then(|i| ThemePref::parse(&p.items[*i].value)),
            ),
            _ => None,
        };
        match picking {
            Some(highlighted) => {
                let saved = *self.theme_saved.get_or_insert(self.theme.pref());
                self.show_theme(highlighted.unwrap_or(saved));
            }
            None => {
                if let Some(saved) = self.theme_saved.take() {
                    self.show_theme(saved);
                }
            }
        }
    }

    /// Housekeeping on timer ticks.
    pub fn tick(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.until)
        {
            self.toast = None;
            self.dirty = true;
        }
        if self
            .last_ctrl_c
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
        {
            self.last_ctrl_c = None;
            self.dirty = true;
        }
        if self.boot == Boot::WaitingForModel && self.boot_since.elapsed() > Duration::from_secs(12)
        {
            self.boot = Boot::NoModel;
            self.dirty = true;
        }
    }

    /// Next instant a timer-driven redraw is needed.
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut d: Option<Instant> = None;
        let mut consider = |t: Instant| d = Some(d.map_or(t, |x: Instant| x.min(t)));
        if self.animating() {
            let ms = self.started.elapsed().as_millis() as u64;
            let step = self.step_ms();
            consider(Instant::now() + Duration::from_millis(step - ms % step));
        }
        if let Some(t) = &self.toast {
            consider(t.until);
        }
        if let Some(t) = self.last_ctrl_c {
            consider(t + Duration::from_millis(1001));
        }
        if self.panes.iter().any(|p| p.turn_started.is_some()) {
            consider(Instant::now() + Duration::from_millis(500));
        }
        if self.boot == Boot::WaitingForModel {
            consider(self.boot_since + Duration::from_millis(12_001));
        }
        d
    }

    // ---- startup & sessions ------------------------------------------------------------

    /// Open the first chat: the one asked for (`-r`), a new one for a
    /// prompt given on the command line, else the most recent chat in
    /// this folder (any chat at all with `-c`), else a new one.
    pub fn start(&mut self, opts: &super::TuiOptions) {
        self.yolo_default = opts.yolo;
        if let Some(m) = &opts.model {
            match m.parse::<ModelRef>() {
                Ok(m) => {
                    self.want_model = Some(m);
                    self.model_explicit = true;
                }
                Err(e) => self.error(format!("--model: {e}")),
            }
        }
        self.refresh_providers();
        self.refresh_bots();
        let pid = self.focus;
        let prompt = opts.prompt.clone().filter(|p| !p.trim().is_empty());
        if let Some(id) = &opts.resume {
            if let Some(p) = self.pane_by_mut(pid) {
                p.pending_prompt = prompt;
            }
            return self.load_session(pid, id.clone(), OnFail::NewChat, true);
        }
        if let Some(prompt) = prompt {
            if let Some(p) = self.pane_by_mut(pid) {
                p.pending_prompt = Some(prompt);
            }
            return self.create_session(pid, NewChat::default());
        }
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let any = opts.continue_last;
        self.call::<Vec<SessionInfo>, _>(
            Request::ListSessions { archived: false },
            move |app, r| {
                let list = r.unwrap_or_default();
                let pick = recent_chat(&list, &cwd).or_else(|| {
                    any.then(|| {
                        list.iter()
                            .filter(|s| listed(s) && s.last_seq > 0)
                            .max_by_key(|s| s.updated_at)
                            .map(|s| s.id.clone())
                    })
                    .flatten()
                });
                match pick {
                    Some(id) => app.load_session(pid, id, OnFail::NewQuietly, true),
                    None => app.create_session(pid, NewChat::default()),
                }
            },
        );
    }

    /// The chat is loaded: apply `--model`, send the prompt waiting for it.
    fn became_ready(&mut self, pid: PaneId, want_model: Option<ModelRef>) {
        self.boot = Boot::Ready;
        if let Some(m) = want_model
            && self
                .pane_by(pid)
                .and_then(|p| p.session.as_ref())
                .is_some_and(|s| s.model != m)
        {
            self.update_session(
                pid,
                Upd {
                    model: Some(m),
                    ..Default::default()
                },
                None,
            );
        }
        if let Some(p) = self.pane_by_mut(pid).and_then(|p| p.pending_prompt.take()) {
            self.send_text(pid, p, Delivery::Auto);
        }
    }

    pub fn create_session(&mut self, pid: PaneId, spec: NewChat) {
        let want = self.want_model.clone();
        let Some(p) = self.pane_by_mut(pid) else {
            return;
        };
        p.creating = true;
        p.epoch += 1;
        let epoch = p.epoch;
        let cwd = spec.cwd.clone().or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        });
        let model = spec.model.clone().or(want);
        let req = Request::CreateSession {
            cwd,
            model: model.clone(),
            title: None,
            bot_id: None,
            private: false,
            // A terminal chat: the lean coding harness.
            lean: true,
        };
        self.call::<SessionInfo, _>(req, move |app, r| {
            if !app.pane_by(pid).is_some_and(|p| p.epoch == epoch) {
                // The pane closed or moved on: don't leave an empty chat behind.
                if let Ok(info) = r {
                    app.fire(Request::DeleteSession {
                        session_id: info.id,
                    });
                }
                return;
            }
            if let Some(p) = app.pane_by_mut(pid) {
                p.creating = false;
            }
            app.created(pid, r, model, spec);
        });
    }

    fn created(
        &mut self,
        pid: PaneId,
        r: Result<SessionInfo, ApiError>,
        model: Option<ModelRef>,
        spec: NewChat,
    ) {
        match r {
            Ok(info) => {
                // `--model` applies to the first chat only.
                self.want_model = None;
                self.discard_if_empty(pid);
                let permission = if self.yolo_default {
                    (info.permission != PermissionMode::Auto).then_some(PermissionMode::Auto)
                } else {
                    self.new_permission.filter(|m| *m != info.permission)
                };
                self.created_here.insert(info.id.clone());
                let hist = self.proc_history.clone();
                if let Some(p) = self.pane_by_mut(pid) {
                    p.reset();
                    p.editor.set_history(hist);
                    p.apply_info(info);
                }
                if permission.is_some() {
                    self.update_session(
                        pid,
                        Upd {
                            permission,
                            ..Default::default()
                        },
                        None,
                    );
                }
                self.became_ready(pid, None);
                self.adopt_default_model(pid);
            }
            Err(e) if e.message.contains("no model configured") => {
                if self.boot != Boot::NoModel {
                    if self.boot != Boot::WaitingForModel {
                        self.boot_since = Instant::now();
                    }
                    self.boot = Boot::WaitingForModel;
                }
                // Discovery runs in the background on first start; retry.
                if self.boot == Boot::WaitingForModel {
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        let _ = tx.send(Box::new(move |app: &mut App| {
                            if app.pane_by(pid).is_some_and(|p| {
                                p.session.is_none() && !p.creating && p.loading.is_none()
                            }) && app.boot == Boot::WaitingForModel
                            {
                                app.create_session(pid, spec);
                            }
                        }));
                    });
                }
            }
            Err(e) if model.is_some() && e.message.contains("unknown") => {
                self.pane_error(pid, format!("{}: using the default model", e.message));
                self.want_model = None;
                self.create_session(
                    pid,
                    NewChat {
                        model: None,
                        ..spec
                    },
                );
            }
            Err(e) => {
                if self.panes.iter().all(|p| p.session.is_none()) {
                    self.boot = Boot::NoModel;
                }
                self.pane_error(pid, format!("Couldn't start a chat: {}", e.message));
            }
        }
    }

    /// Show chat `id` in pane `pid`. The pane keeps its current chat until
    /// the chat arrives.
    pub(super) fn load_session(
        &mut self,
        pid: PaneId,
        id: String,
        on_fail: OnFail,
        apply_model: bool,
    ) {
        let Some(p) = self.pane_by_mut(pid) else {
            return;
        };
        p.epoch += 1;
        p.loading = Some(id.clone());
        let epoch = p.epoch;
        self.call::<SessionSnapshot, _>(
            Request::GetSession {
                session_id: id.clone(),
                since_seq: None,
            },
            move |app, r| {
                if !app.pane_by(pid).is_some_and(|p| p.epoch == epoch) {
                    return;
                }
                if let Some(p) = app.pane_by_mut(pid) {
                    p.loading = None;
                }
                match r {
                    Ok(snap) => {
                        app.discard_if_empty(pid);
                        let hist = app.proc_history.clone();
                        let mut v = app.version;
                        if let Some(p) = app.pane_by_mut(pid) {
                            p.reset();
                            p.apply_snapshot(snap, true, &hist, &mut v);
                        }
                        app.version = v;
                        let want = if apply_model {
                            app.want_model.take()
                        } else {
                            None
                        };
                        app.became_ready(pid, want);
                        app.tell_app_tools(pid);
                        app.fetch_session_boards(&id);
                    }
                    Err(e) => match on_fail {
                        OnFail::Report => {
                            app.pane_error(pid, format!("Couldn't open that chat: {}", e.message))
                        }
                        OnFail::NewChat => {
                            app.pane_error(pid, format!("Couldn't open chat {id}: {}", e.message));
                            app.create_session(pid, NewChat::default());
                        }
                        OnFail::NewQuietly => app.create_session(pid, NewChat::default()),
                    },
                }
            },
        );
    }

    /// A chat from the apps keeps its own tools: say so once, plainly,
    /// when it can do things the terminal doesn't show.
    fn tell_app_tools(&mut self, pid: PaneId) {
        let Some(s) = self.pane_by(pid).and_then(|p| p.session.clone()) else {
            return;
        };
        if s.lean || s.private || !self.told_app_tools.insert(s.id.clone()) {
            return;
        }
        let what = match s.kind {
            SessionKind::Channel => {
                "This is a room from the apps: its bots keep their own tools. You can read it and write in it here."
            }
            SessionKind::Chat if s.bot_id.is_some() => {
                "This chat is with a bot from the apps and keeps the bot's tools. Whiteboards, notes and web views it opens show in the apps."
            }
            SessionKind::Chat => {
                "This chat was started in the apps and keeps all of its tools. Whiteboards, notes and web views it opens show in the apps, not here."
            }
            // Subagents and scheduled runs: theirs are what they were given.
            _ => return,
        };
        if let Some(p) = self.pane_by_mut(pid) {
            p.transcript.add_local(NoticeLevel::Info, what);
        }
    }

    /// Delete the pane's chat if this process created it and nothing was
    /// ever sent (keeps `/chats` free of empty chats).
    pub(super) fn discard_if_empty(&mut self, pid: PaneId) {
        let Some(p) = self.pane_by(pid) else { return };
        let Some(s) = &p.session else { return };
        let shown_elsewhere = self.panes.iter().any(|o| o.id != pid && o.shows(&s.id));
        if self.created_here.contains(&s.id)
            && s.last_seq == 0
            && p.transcript.is_empty()
            && !p.busy()
            && !shown_elsewhere
        {
            self.fire(Request::DeleteSession {
                session_id: s.id.clone(),
            });
        }
    }

    /// Called on exit.
    pub async fn shutdown(&mut self) {
        self.save_prefs();
        let mut aborted = false;
        let mut seen = HashSet::new();
        for p in &self.panes {
            let Some(s) = &p.session else { continue };
            if !seen.insert(s.id.clone()) {
                continue;
            }
            if self.created_here.contains(&s.id)
                && s.last_seq == 0
                && p.transcript.is_empty()
                && !p.busy()
            {
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    self.backend.call(Request::DeleteSession {
                        session_id: s.id.clone(),
                    }),
                )
                .await;
            } else if p.busy() && matches!(self.backend, Backend::Local(_)) {
                // The in-process engine dies with us: stop the turn cleanly.
                let _ = self
                    .backend
                    .call(Request::Abort {
                        session_id: s.id.clone(),
                    })
                    .await;
                aborted = true;
            }
        }
        if aborted {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    fn resync(&mut self) {
        let ids: Vec<PaneId> = self.panes.iter().map(|p| p.id).collect();
        for pid in ids {
            self.resync_pane(pid);
        }
    }

    fn resync_pane(&mut self, pid: PaneId) {
        let Some(p) = self.pane_by(pid) else { return };
        let Some(id) = p.session_id().map(str::to_string) else {
            return;
        };
        let since = p.transcript.last_seq();
        let epoch = p.epoch;
        self.call::<SessionSnapshot, _>(
            Request::GetSession {
                session_id: id,
                since_seq: Some(since),
            },
            move |app, r| {
                let hist = app.proc_history.clone();
                let mut v = app.version;
                match r {
                    Ok(snap) => {
                        if let Some(p) = app.pane_by_mut(pid).filter(|p| p.epoch == epoch) {
                            p.apply_snapshot(snap, false, &hist, &mut v);
                        }
                    }
                    Err(e) => app.pane_error(pid, format!("Couldn't catch up: {}", e.message)),
                }
                app.version = v;
            },
        );
    }

    pub fn refresh_providers(&mut self) {
        self.call::<Vec<ProviderInfo>, _>(Request::ListProviders { refresh: false }, |app, r| {
            if let Ok(p) = r {
                app.providers = p;
            }
        });
        self.call::<ServerInfo, _>(
            Request::Hello {
                client: "tui".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            |app, r| {
                if let Ok(info) = r {
                    app.server = Some(info);
                    let ids: Vec<PaneId> = app.panes.iter().map(|p| p.id).collect();
                    for pid in ids {
                        app.adopt_default_model(pid);
                    }
                }
            },
        );
    }

    /// Bots name who speaks in chats from the apps (the terminal has none
    /// of its own).
    pub fn refresh_bots(&mut self) {
        self.call::<Vec<Bot>, _>(Request::ListBots, |app, r| {
            if let Ok(list) = r {
                let t = app.theme;
                app.bot_looks = list
                    .iter()
                    .map(|b| (b.id.clone(), BotLook::new(b, &t)))
                    .collect();
            }
        });
    }

    /// On first run the engine finds local model servers in the background,
    /// possibly after our chat was created with a fallback model. While
    /// the chat is untouched, follow the server's default.
    fn adopt_default_model(&mut self, pid: PaneId) {
        if self.model_explicit {
            return;
        }
        let Some(p) = self.pane_by(pid) else { return };
        let Some(s) = &p.session else { return };
        if !self.created_here.contains(&s.id)
            || s.last_seq > 0
            || !p.transcript.is_empty()
            || p.busy()
        {
            return;
        }
        let Some(def) = self.server.as_ref().and_then(|i| i.default_model.clone()) else {
            return;
        };
        if def != s.model {
            self.update_session(
                pid,
                Upd {
                    model: Some(def),
                    ..Default::default()
                },
                None,
            );
        }
    }

    // ---- backend events ----------------------------------------------------------------

    pub fn on_backend(&mut self, ev: BackendEvent) {
        self.dirty = true;
        match ev {
            BackendEvent::Status(s) => {
                self.conn = s.state;
            }
            BackendEvent::Reconnected => {
                self.conn = ConnState::Connected;
                self.resync();
                self.refresh_providers();
            }
            BackendEvent::Event(e) => self.on_event(e),
        }
    }

    pub fn on_event(&mut self, e: Event) {
        match &e {
            Event::SessionRewound { session_id, to_seq } => {
                // Another device (or this one) rewound it: drop what's gone
                // and fetch the rest.
                let pids: Vec<PaneId> = self
                    .panes
                    .iter()
                    .filter(|p| p.session_id() == Some(session_id.as_str()))
                    .map(|p| p.id)
                    .collect();
                for pid in pids {
                    if let Some(p) = self.pane_by_mut(pid) {
                        p.transcript.truncate_after(*to_seq);
                        p.think_ms.retain(|s, _| s <= to_seq);
                        p.live = None;
                        p.live_cache = Default::default();
                        p.scroll = 0;
                    }
                    self.resync_pane(pid);
                }
                return;
            }
            Event::Resync => return self.resync(),
            Event::ProvidersChanged => {
                self.refresh_providers();
                if matches!(self.boot, Boot::WaitingForModel | Boot::NoModel) {
                    let idle: Vec<PaneId> = self
                        .panes
                        .iter()
                        .filter(|p| p.session.is_none() && !p.creating && p.loading.is_none())
                        .map(|p| p.id)
                        .collect();
                    if !idle.is_empty() {
                        self.boot = Boot::WaitingForModel;
                        self.boot_since = Instant::now();
                    }
                    for pid in idle {
                        self.create_session(pid, NewChat::default());
                    }
                }
                return;
            }
            Event::BotUpdated { bot } => {
                let look = BotLook::new(bot, &self.theme);
                self.bot_looks.insert(bot.id.clone(), look);
                return;
            }
            Event::BotRemoved { id } => {
                self.bot_looks.remove(id);
                return;
            }
            Event::BoardUpdated { board } => return self.board_arrived(board.clone()),
            Event::BoardRemoved { id, .. } => {
                self.boards.remove(id);
                return;
            }
            Event::Notice {
                session_id: None,
                level,
                message,
            } => {
                match level {
                    NoticeLevel::Info => self.toast(message.clone()),
                    _ => self
                        .pane_mut()
                        .transcript
                        .add_local(*level, message.clone()),
                }
                return;
            }
            _ => {}
        }
        let mut version = self.version;
        let mut effects = Vec::new();
        let mut rewritten = Vec::new();
        for p in self.panes.iter_mut() {
            let fx = p.on_event(&e, &mut version);
            if fx != Effect::None {
                effects.push((p.id, fx));
            }
            rewritten.append(&mut p.rewritten);
        }
        self.version = version;
        // Typed text the server rewrote (a secret taken out) leaves the
        // history new chats start from too.
        for (typed, stored) in rewritten {
            for h in self.proc_history.iter_mut().filter(|h| **h == typed) {
                *h = stored.clone();
            }
        }
        for (pid, fx) in effects {
            match fx {
                Effect::Toast(m) => self.toast(m),
                Effect::TurnEnded => {
                    if pid == self.focus
                        && self
                            .toast
                            .as_ref()
                            .is_some_and(|t| t.text == "Interrupting…")
                    {
                        self.toast = None;
                    }
                }
                Effect::Removed => self.pane_error(
                    pid,
                    "This chat was deleted on another device. Ctrl+N starts a new one.",
                ),
                Effect::None => {}
            }
        }
    }

    // ---- sending -----------------------------------------------------------------------

    fn send_text(&mut self, pid: PaneId, text_s: String, delivery: Delivery) {
        let conn = self.conn;
        let Some(p) = self.pane_by_mut(pid) else {
            return;
        };
        if p.is_dm() {
            if p.editor.is_empty() {
                p.editor.set_text(text_s);
            }
            self.toast_warn("A private conversation between bots is read-only");
            return;
        }
        let Some(sid) = p.session_id().map(str::to_string) else {
            p.pending_prompt = Some(text_s);
            self.toast("Your message goes out as soon as the chat is ready");
            return;
        };
        if conn != ConnState::Connected {
            if p.editor.is_empty() {
                p.editor.set_text(text_s);
            }
            self.pane_error(pid, "Not connected to the server: the message wasn't sent.");
            return;
        }
        p.editor.push_history(&text_s);
        p.sent = Some(text_s.clone());
        p.scroll = 0;
        let busy = p.busy();
        if self.proc_history.last() != Some(&text_s) {
            self.proc_history.push(text_s.clone());
        }
        let note = match delivery {
            Delivery::Auto | Delivery::FollowUp if busy => {
                Some("Queued: it goes in after this reply (↑ edits or steers it)")
            }
            Delivery::Steer => Some("Steering: the agent reads it at its next step"),
            _ => None,
        };
        if let Some(m) = note {
            self.toast(m);
        }
        let restore = text_s.clone();
        self.call::<Value, _>(
            Request::Send {
                session_id: sid,
                content: vec![ContentPart::text(text_s)],
                delivery,
                attachments: Vec::new(),
            },
            move |app, r| {
                if let Err(e) = r {
                    app.pane_error(pid, format!("Message not sent: {}", e.message));
                    if let Some(p) = app.pane_by_mut(pid)
                        && p.editor.is_empty()
                    {
                        p.editor.set_text(restore);
                    }
                }
            },
        );
    }

    /// Send the composer. While the agent works the server queues it (↑
    /// then edits or steers it); `steer` delivers it at the next step.
    fn submit(&mut self, steer: bool) {
        let t = self.pane().editor.text().to_string();
        if t.trim().is_empty() {
            return;
        }
        if is_command_line(&t) {
            let name = t.split_whitespace().next().unwrap_or("");
            if commands::find(name).is_none() {
                let hint = match commands::closest(name) {
                    Some(c) => format!(" Did you mean /{}? (/help lists them all)", c.name),
                    None => " /help lists every command.".to_string(),
                };
                self.error(format!("There is no {name} command.{hint}"));
                return;
            }
            let p = self.pane_mut();
            p.editor.push_history(t.trim());
            p.editor.clear();
            p.completion = None;
            self.run_command(t.trim());
            return;
        }
        if let Some(index) = self.pane().editing_queued {
            return self.save_queued_edit(index, t.trim_end().to_string());
        }
        let p = self.pane_mut();
        p.editor.clear();
        p.completion = None;
        let delivery = if steer && p.busy() {
            Delivery::Steer
        } else {
            Delivery::Auto
        };
        let pid = self.focus;
        self.send_text(pid, t.trim_end().to_string(), delivery);
    }

    // ---- terminal input ------------------------------------------------------------------

    pub fn on_terminal(&mut self, ev: TermEvent) {
        self.terminal_event(ev);
        self.preview_theme();
    }

    fn terminal_event(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) => self.on_key(k),
            TermEvent::Paste(s) => {
                self.dirty = true;
                let s = s.replace("\r\n", "\n").replace('\r', "\n");
                let clean = text::sanitize(&s).into_owned();
                match &mut self.modal {
                    Some(Modal::Picker(p)) if p.filterable() => {
                        p.filter.push_str(clean.lines().next().unwrap_or(""));
                        p.selected = 0;
                    }
                    Some(_) => {}
                    None => {
                        if self.pane().approvals.is_empty() {
                            self.pane_mut().editor.insert_str(&clean);
                            self.update_completion();
                        }
                    }
                }
            }
            TermEvent::Mouse(m) => self.on_mouse(m),
            TermEvent::Resize(w, h) => {
                self.screen = Rect::new(0, 0, w, h);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<PaneId> {
        let pos = ratatui::layout::Position { x, y };
        self.hits
            .iter()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, Hit::Pane(id))| *id)
    }

    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                match &mut self.modal {
                    Some(Modal::Info(i)) => {
                        i.scroll = if up {
                            i.scroll.saturating_sub(3)
                        } else {
                            i.scroll + 3
                        };
                    }
                    Some(Modal::Picker(_)) => {
                        let code = if up { KeyCode::Up } else { KeyCode::Down };
                        self.modal_key(KeyEvent::new(code, KeyModifiers::NONE));
                    }
                    Some(_) => {}
                    None => {
                        let pid = self.pane_at(m.column, m.row).unwrap_or(self.focus);
                        self.scroll_pane(pid, if up { 3 } else { -3 });
                    }
                }
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.dirty = true;
                if self.modal.is_some() {
                    return self.modal_click(m.column, m.row);
                }
                if let Some(id) = self.pane_at(m.column, m.row) {
                    self.focus_pane(id);
                }
            }
            _ => {}
        }
    }

    /// A click in a popup: the first click on an item selects it, a click
    /// on the selected one uses it (like ⏎); outside a list closes it.
    fn modal_click(&mut self, x: u16, y: u16) {
        let hits = self.modal_hits.clone();
        if !hits.popup.contains(ratatui::layout::Position { x, y }) {
            if matches!(self.modal, Some(Modal::Picker(_) | Modal::Info(_))) {
                self.close_modal();
            }
            return;
        }
        let Some(i) = hits.item(x, y) else { return };
        if let Some(Modal::Picker(p)) = self.modal.as_mut() {
            if p.selected == i {
                self.modal_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            } else {
                p.selected = i;
            }
        }
    }

    fn scroll_pane(&mut self, pid: PaneId, delta: isize) {
        if let Some(p) = self.pane_by_mut(pid) {
            p.scroll = if delta > 0 {
                p.scroll.saturating_add(delta as usize)
            } else {
                p.scroll.saturating_sub(delta.unsigned_abs())
            };
        }
        self.dirty = true;
    }

    fn scroll_by(&mut self, delta: isize) {
        let f = self.focus;
        self.scroll_pane(f, delta);
    }

    pub(super) fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        if ctrl && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            return self.ctrl_c();
        }
        if self.modal.is_some() {
            return self.modal_key(k);
        }
        match k.code {
            KeyCode::F(1) => return self.open_help(),
            KeyCode::F(2) => return self.open_settings(),
            _ => {}
        }
        if alt && !ctrl {
            let several = self.panes.len() > 1;
            match k.code {
                // Moving between chats side by side; with one chat,
                // alt+←/→ keep moving by word.
                KeyCode::Left | KeyCode::Char('h' | 'H') if several => {
                    return self.focus_step(-1);
                }
                KeyCode::Right | KeyCode::Char('l' | 'L') if several => {
                    return self.focus_step(1);
                }
                // With a message typed while the agent works: steer.
                KeyCode::Enter
                    if self.pane().busy()
                        && !self.pane().editor.is_empty()
                        && self.pane().approvals.is_empty()
                        && self.pane().editing_queued.is_none() =>
                {
                    return self.submit(true);
                }
                _ => {}
            }
        }
        if ctrl && !alt && matches!(k.code, KeyCode::Char('n')) {
            return self.new_chat_here();
        }
        match k.code {
            KeyCode::Char('o') if ctrl => {
                self.expand = !self.expand;
                self.toast(if self.expand {
                    "Showing every step and its output (ctrl+o folds them)"
                } else {
                    "Steps folded (ctrl+o shows them all)"
                });
                return;
            }
            KeyCode::Char('t') if ctrl => {
                self.set_show_thinking(!self.show_thinking);
                return;
            }
            KeyCode::Char('l') if ctrl => {
                self.plan_collapsed = !self.plan_collapsed;
                self.force_clear = true;
                return;
            }
            KeyCode::Char('z') if ctrl && cfg!(unix) => {
                self.suspend = true;
                return;
            }
            KeyCode::Char('d') if ctrl && self.pane().editor.is_empty() => {
                self.quit = true;
                return;
            }
            KeyCode::BackTab => return self.cycle_thinking(),
            KeyCode::Tab if shift => return self.cycle_thinking(),
            KeyCode::PageUp => return self.scroll_by(self.pane().page as isize),
            KeyCode::PageDown => return self.scroll_by(-(self.pane().page as isize)),
            KeyCode::Up if shift => return self.scroll_by(1),
            KeyCode::Down if shift => return self.scroll_by(-1),
            KeyCode::Home if ctrl => {
                self.pane_mut().scroll = usize::MAX / 2;
                return;
            }
            KeyCode::End if ctrl => {
                self.pane_mut().scroll = 0;
                return;
            }
            _ => {}
        }
        if !self.pane().approvals.is_empty() {
            return self.approval_key(k);
        }
        if self.ask_key(k) {
            return;
        }
        if self.queue_key(k) {
            return;
        }
        if self.pane().completion.is_some() && !ctrl && !alt {
            match k.code {
                KeyCode::Up => return self.move_completion(-1),
                KeyCode::Down => return self.move_completion(1),
                KeyCode::Tab => return self.accept_completion(false),
                KeyCode::Enter if !shift => return self.accept_completion(true),
                KeyCode::Esc => {
                    let p = self.pane_mut();
                    p.comp_dismissed = Some(p.editor.text().to_string());
                    p.completion = None;
                    return;
                }
                _ => {}
            }
        }
        let w = self.pane().input_width;
        match k.code {
            KeyCode::Enter if shift => self.pane_mut().editor.insert_char('\n'),
            KeyCode::Enter if ctrl => self.submit(false),
            KeyCode::Enter => {
                let e = &mut self.pane_mut().editor;
                if e.text()[..e.cursor()].ends_with('\\') {
                    e.backspace();
                    e.insert_char('\n');
                } else {
                    self.submit(false);
                }
            }
            KeyCode::Esc => {
                let pid = self.focus;
                let p = self.pane_mut();
                if p.editing_queued.is_some() {
                    p.editing_queued = None;
                    p.editor.clear();
                    self.toast("Edit cancelled; the queued message is unchanged");
                } else if p.busy() {
                    self.abort(pid);
                } else if !p.editor.is_empty() {
                    let t = p.editor.take();
                    p.editor.push_history(&t);
                } else if p.scroll > 0 {
                    p.scroll = 0;
                }
            }
            // `?` on an empty line is the help (an answer being typed may
            // start with one).
            KeyCode::Char('?')
                if !ctrl
                    && !alt
                    && self.pane().editor.is_empty()
                    && super::ask::pending_ask(self.pane(), &self.boards).is_none() =>
            {
                self.open_help()
            }
            _ => {
                let single = self.panes.len() <= 1;
                edit_key(&mut self.pane_mut().editor, k, ctrl, alt, w, single);
            }
        }
        self.update_completion();
    }

    fn ctrl_c(&mut self) {
        if self.modal.is_some() {
            self.modal = None;
            return;
        }
        let armed = self.ctrl_c_armed();
        let pid = self.focus;
        let p = self.pane_mut();
        if p.completion.is_some() {
            p.comp_dismissed = Some(p.editor.text().to_string());
            p.completion = None;
        }
        if !p.editor.is_empty() {
            let t = p.editor.take();
            p.editor.push_history(&t);
            self.last_ctrl_c = None;
            return;
        }
        if armed {
            self.quit = true;
            return;
        }
        if p.busy() {
            self.abort(pid);
        }
        self.last_ctrl_c = Some(Instant::now());
    }

    pub fn ctrl_c_armed(&self) -> bool {
        self.last_ctrl_c
            .is_some_and(|t| t.elapsed() <= Duration::from_secs(1))
    }

    fn abort(&mut self, pid: PaneId) {
        let Some(sid) = self
            .pane_by(pid)
            .and_then(|p| p.session_id())
            .map(str::to_string)
        else {
            return;
        };
        self.toast_warn("Interrupting…");
        self.call::<Value, _>(Request::Abort { session_id: sid }, move |app, r| {
            if let Err(e) = r {
                app.pane_error(pid, format!("Couldn't interrupt: {}", e.message));
            }
        });
    }

    fn approval_key(&mut self, k: KeyEvent) {
        // A long one scrolls (the card keeps the part shown and how much is
        // below as of its last drawing).
        let v = &mut self.pane_mut().approval_view;
        let scroll = match k.code {
            KeyCode::Down => Some(v.scroll + v.below.min(1)),
            KeyCode::Char(' ') => Some(v.scroll + v.below.min(v.rows.max(1))),
            KeyCode::Up => Some(v.scroll.saturating_sub(1)),
            KeyCode::End => Some(v.scroll + v.below),
            KeyCode::Home => Some(0),
            _ => None,
        };
        if let Some(s) = scroll {
            v.scroll = s;
            return;
        }
        // Keys typed as the card appears were meant for the message box: a
        // "y" or "a" mid-word must not allow anything.
        if self
            .pane()
            .approval_at
            .is_some_and(|t| t.elapsed() < APPROVAL_GUARD)
        {
            return self.toast("An approval is waiting: y allow · a allow for this chat · n deny");
        }
        let Some(req) = self.pane().approvals.first().cloned() else {
            return;
        };
        let decision = match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalDecision::Allow,
            KeyCode::Char('a') | KeyCode::Char('A') if !req.once => ApprovalDecision::AllowSession,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => ApprovalDecision::Deny,
            _ => return,
        };
        // Nothing is allowed unseen: all of it must have been on screen.
        let v = &self.pane().approval_view;
        if decision != ApprovalDecision::Deny && !(v.id == req.id && v.read) {
            return self.toast_warn("Read all of it first: ↓ shows the rest (n denies)");
        }
        let pid = self.focus;
        let p = self.pane_mut();
        let Some(sid) = p.session_id().map(str::to_string) else {
            return;
        };
        p.approvals.remove(0);
        // The next one (if any) gets its own moment.
        p.approval_at = (!p.approvals.is_empty()).then(Instant::now);
        let label = match decision {
            ApprovalDecision::Allow => "Allowed",
            ApprovalDecision::AllowSession => "Allowed for this chat",
            ApprovalDecision::Deny => "Denied",
        };
        self.toast(format!("{label}: {}", text::truncate(&req.summary, 60)));
        self.call::<Value, _>(
            Request::RespondApproval {
                session_id: sid,
                request_id: req.id.clone(),
                decision,
            },
            move |app, r| {
                if let Err(e) = r
                    && e.code != ErrorCode::NotFound
                {
                    app.pane_error(pid, format!("The answer didn't arrive: {}", e.message));
                }
            },
        );
    }

    /// The reasoning modes the focused chat's model really supports.
    fn model_levels(&self) -> Vec<ThinkingOption> {
        let Some(s) = &self.pane().session else {
            return Vec::new();
        };
        self.model_info(&s.model)
            .map(|m| m.thinking.clone())
            .unwrap_or_default()
    }

    fn cycle_thinking(&mut self) {
        let Some(current) = self.pane().session.as_ref().map(|s| s.thinking) else {
            return;
        };
        let levels: Vec<ThinkingLevel> = self.model_levels().iter().map(|o| o.level).collect();
        if levels.is_empty() {
            self.toast("This model has no thinking levels");
            return;
        }
        let i = levels
            .iter()
            .position(|t| *t == current)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.set_thinking(levels[i % levels.len()]);
    }

    fn set_thinking(&mut self, t: ThinkingLevel) {
        if let Some(s) = self.pane_mut().session.as_mut() {
            s.thinking = t;
        }
        let pid = self.focus;
        self.update_session(
            pid,
            Upd {
                thinking: Some(t),
                ..Default::default()
            },
            Some(format!("Thinking: {}", thinking_name(t))),
        );
    }

    /// The permission preset for this chat, remembered for new ones.
    pub(super) fn set_permission(&mut self, m: PermissionMode) {
        self.new_permission = (m != PermissionMode::Default).then_some(m);
        self.save_prefs();
        let msg = format!("{}: {}", perm_label(m), perm_desc(m));
        if self.pane().session.is_none() {
            return self.toast(msg);
        }
        if let Some(s) = self.pane_mut().session.as_mut() {
            s.permission = m;
        }
        let pid = self.focus;
        self.update_session(
            pid,
            Upd {
                permission: Some(m),
                ..Default::default()
            },
            Some(msg),
        );
    }

    /// YOLO is explained before it is turned on.
    pub(super) fn choose_permission(&mut self, m: PermissionMode) {
        let now = self
            .pane()
            .session
            .as_ref()
            .map(|s| s.permission)
            .or(self.new_permission);
        if m == PermissionMode::Auto && now != Some(PermissionMode::Auto) {
            self.modal = Some(Modal::Confirm(Confirm {
                title: "Turn on YOLO?".into(),
                text: "The agent will edit files, run commands and go online without asking you, in this chat and new ones. Catastrophic commands are still blocked.".into(),
                action: ConfirmAction::Permission(m),
            }));
            return;
        }
        self.set_permission(m);
    }

    fn set_model(&mut self, m: ModelRef) {
        self.model_explicit = true;
        let pid = self.focus;
        self.update_session(
            pid,
            Upd {
                model: Some(m.clone()),
                ..Default::default()
            },
            Some(format!("Model: {}", m.model)),
        );
    }

    fn update_session(&mut self, pid: PaneId, u: Upd, ok: Option<String>) {
        let Some(sid) = self
            .pane_by(pid)
            .and_then(|p| p.session_id())
            .map(str::to_string)
        else {
            self.toast_warn("The chat isn't ready yet");
            return;
        };
        let req = Request::UpdateSession {
            session_id: sid,
            title: None,
            model: u.model,
            thinking: u.thinking,
            permission: u.permission,
            pinned: None,
            archived: None,
            cwd: None,
            members: None,
            fast: None,
            subagents: None,
        };
        self.call::<SessionInfo, _>(req, move |app, r| match r {
            Ok(info) => {
                for p in app
                    .panes
                    .iter_mut()
                    .filter(|p| p.session_id() == Some(info.id.as_str()))
                {
                    p.apply_info(info.clone());
                }
                if let Some(m) = ok {
                    app.toast(m);
                }
            }
            Err(e) => app.pane_error(pid, e.message),
        });
    }

    // ---- chats side by side ------------------------------------------------------------

    pub fn focus_pane(&mut self, id: PaneId) {
        if self.pane_by(id).is_none() {
            return;
        }
        if self.focus != id {
            self.pane_mut().completion = None;
            self.focus = id;
            self.last_ctrl_c = None;
        }
        self.dirty = true;
    }

    /// Move the keyboard to the chat on the left (-1) or right (+1).
    fn focus_step(&mut self, d: isize) {
        let n = self.panes.len() as isize;
        let i = self
            .panes
            .iter()
            .position(|p| p.id == self.focus)
            .unwrap_or(0) as isize;
        let next = self.panes[((i + d).rem_euclid(n)) as usize].id;
        self.focus_pane(next);
    }

    /// How many chats fit side by side in this window.
    pub fn room_for(&self) -> usize {
        ((self.screen.width / MIN_PANE_W) as usize).clamp(1, MAX_PANES)
    }

    /// `/split`: a new chat beside this one (same folder and model), which
    /// gets the keyboard.
    pub fn split(&mut self) {
        if self.panes.len() >= MAX_PANES {
            return self.toast_warn(format!("Up to {MAX_PANES} chats side by side"));
        }
        if self.panes.len() >= self.room_for() {
            return self.toast_warn(if self.panes.len() == 1 {
                "Too narrow for chats side by side: widen the window"
            } else {
                "No room for another chat: widen the window or /close one"
            });
        }
        let spec = self.like_this_chat();
        let id = self.next_pane;
        self.next_pane += 1;
        let at = self
            .panes
            .iter()
            .position(|p| p.id == self.focus)
            .map_or(self.panes.len(), |i| i + 1);
        self.panes.insert(at, Pane::new(id));
        self.focus_pane(id);
        self.force_clear = true;
        self.create_session(id, spec);
    }

    /// `/close`: close this chat beside the others (it stays in `/chats`).
    pub fn close_pane(&mut self) {
        if self.panes.len() <= 1 {
            return self.toast("This is your only chat: /new starts another, /quit leaves");
        }
        let pid = self.focus;
        self.discard_if_empty(pid);
        let i = self.panes.iter().position(|p| p.id == pid).unwrap_or(0);
        self.panes.remove(i);
        self.focus = self.panes[i.saturating_sub(1).min(self.panes.len() - 1)].id;
        self.force_clear = true;
        self.dirty = true;
    }

    /// A new chat like the focused one: same folder and model.
    fn like_this_chat(&self) -> NewChat {
        match &self.pane().session {
            Some(s) if s.kind == SessionKind::Chat && s.bot_id.is_none() && !s.private => NewChat {
                cwd: Some(s.cwd.clone()),
                model: Some(s.model.clone()),
            },
            _ => NewChat::default(),
        }
    }

    /// Ctrl+N and `/new`: a new chat in place of this one.
    pub(super) fn new_chat_here(&mut self) {
        let spec = self.like_this_chat();
        let pid = self.focus;
        self.create_session(pid, spec);
    }

    /// Open chat `id` here (or give the keyboard to the pane showing it).
    pub fn open_chat(&mut self, id: String) {
        if let Some(existing) = self.panes.iter().find(|p| p.shows(&id)).map(|p| p.id) {
            return self.focus_pane(existing);
        }
        let f = self.focus;
        self.load_session(f, id, OnFail::Report, false);
    }

    // ---- completion ------------------------------------------------------------------

    fn move_completion(&mut self, d: isize) {
        if let Some(c) = self.pane_mut().completion.as_mut() {
            let n = c.items.len() as isize;
            if n > 0 {
                c.selected = ((c.selected as isize + d).rem_euclid(n)) as usize;
            }
        }
    }

    fn accept_completion(&mut self, enter: bool) {
        let p = self.pane_mut();
        let Some(c) = p.completion.take() else { return };
        let Some(item) = c.items.get(c.selected) else {
            return;
        };
        match c.kind {
            CompKind::Command => {
                let bare = commands::find(&item.value).is_some_and(|c| c.bare);
                if enter && bare {
                    p.editor.replace_range(c.range.clone(), &item.value);
                    self.submit(false);
                    return;
                }
                p.editor
                    .replace_range(c.range.clone(), &format!("{} ", item.value));
            }
            CompKind::Arg => {
                p.editor.replace_range(c.range.clone(), &item.value);
                if enter {
                    self.submit(false);
                    return;
                }
            }
            CompKind::File => {
                let v = if item.dir {
                    item.value.clone()
                } else {
                    format!("{} ", item.value)
                };
                p.editor.replace_range(c.range.clone(), &v);
            }
        }
        self.update_completion();
    }

    pub fn update_completion(&mut self) {
        let p = self.pane();
        let text_s = p.editor.text().to_string();
        if p.comp_dismissed.as_deref() == Some(text_s.as_str()) {
            self.pane_mut().completion = None;
            return;
        }
        let prev_sel = p.completion.as_ref().map(|c| c.selected).unwrap_or(0);
        let prev_kind = p.completion.as_ref().map(|c| c.kind);
        let cursor = p.editor.cursor();
        let (tok_start, tok) = {
            let (s, t) = p.editor.token_before_cursor();
            (s, t.to_string())
        };
        let mut next: Option<Completion> = None;
        let is_cmd = text_s.starts_with('/') && !text_s.contains('\n');
        if is_cmd {
            let first_end = text_s.find(char::is_whitespace).unwrap_or(text_s.len());
            if cursor <= first_end {
                let typed = &text_s[..first_end];
                let found = commands::matching(typed);
                // The arguments column only while every description still
                // fits beside it (a narrow window shows the descriptions).
                let label_w = found.iter().map(|c| c.name.len() + 1).max().unwrap_or(0);
                let desc_w = found.iter().map(|c| text::width(c.desc)).max().unwrap_or(0);
                let room = (p.input_rect.width as usize).saturating_sub(label_w + 7);
                let with_args = room >= commands::ARGS_W + 2 + desc_w;
                let items: Vec<CompItem> = found
                    .into_iter()
                    .map(|c| CompItem {
                        label: format!("/{}", c.name),
                        detail: if with_args {
                            format!("{:<w$}  {}", c.args, c.desc, w = commands::ARGS_W)
                        } else {
                            c.desc.to_string()
                        },
                        value: format!("/{}", c.name),
                        dir: false,
                    })
                    .collect();
                if !items.is_empty()
                    && !(items.len() == 1 && typed == items[0].value && text_s.len() > first_end)
                {
                    next = Some(Completion {
                        kind: CompKind::Command,
                        items,
                        selected: 0,
                        range: 0..first_end,
                    });
                }
            } else {
                next = self.arg_completion(&text_s, first_end);
            }
        }
        if next.is_none()
            && !is_cmd
            && let Some(q) = tok.strip_prefix('@')
        {
            let items = self.file_items(q);
            let exact = items.len() == 1 && items[0].value == tok && !items[0].dir;
            if !items.is_empty() && !exact {
                next = Some(Completion {
                    kind: CompKind::File,
                    items,
                    selected: 0,
                    range: tok_start..cursor,
                });
            }
        }
        if let Some(n) = next.as_mut()
            && Some(n.kind) == prev_kind
        {
            n.selected = prev_sel.min(n.items.len().saturating_sub(1));
        }
        let p = self.pane_mut();
        p.comp_dismissed = None;
        p.completion = next;
    }

    fn arg_completion(&self, text_s: &str, first_end: usize) -> Option<Completion> {
        let name = &text_s[..first_end];
        let cmd = commands::find(name)?;
        let arg = text_s[first_end..].trim_start();
        if arg.contains(char::is_whitespace) {
            return None;
        }
        let mk = |label: String, detail: String| CompItem {
            value: format!("/{} {}", cmd.name, label),
            label,
            detail,
            dir: false,
        };
        let items: Vec<CompItem> = match cmd.name {
            "thinking" => [
                ("show", "Show the model's thinking"),
                ("hide", "Hide the model's thinking"),
            ]
            .iter()
            .map(|(n, d)| (n.to_string(), d.to_string()))
            .chain(self.model_levels().iter().map(|o| {
                (
                    thinking_name(o.level).to_string(),
                    format!("{}: {}", o.label, o.hint),
                )
            }))
            .filter(|(n, _)| n.starts_with(arg))
            .map(|(n, d)| mk(n, d))
            .collect(),
            "goal" if !arg.is_empty() => ["clear"]
                .iter()
                .filter(|n| n.starts_with(arg))
                .map(|n| mk(n.to_string(), "Remove the goal".into()))
                .collect(),
            "model" => {
                let mut v: Vec<(i32, CompItem)> = self
                    .providers
                    .iter()
                    .flat_map(|p| p.models.iter().map(move |m| (p, m)))
                    .filter_map(|(p, m)| {
                        let full = format!("{}/{}", p.id, m.id);
                        // Which account it is on (several ChatGPT or Claude ones), and its window.
                        commands::fuzzy(arg, &full).map(|s| {
                            (
                                s,
                                mk(
                                    full,
                                    format!(
                                        "{} · {} context",
                                        p.name,
                                        text::tokens(m.context_window)
                                    ),
                                ),
                            )
                        })
                    })
                    .collect();
                if !arg.is_empty() {
                    v.sort_by_key(|a| std::cmp::Reverse(a.0));
                }
                v.into_iter().map(|(_, i)| i).take(50).collect()
            }
            _ => return None,
        };
        if items.is_empty() || (items.len() == 1 && items[0].label == arg) {
            return None;
        }
        Some(Completion {
            kind: CompKind::Arg,
            items,
            selected: 0,
            range: 0..text_s.len(),
        })
    }

    fn base_dir(&self) -> PathBuf {
        self.pane()
            .session
            .as_ref()
            .map(|s| PathBuf::from(&s.cwd))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn file_items(&mut self, q: &str) -> Vec<CompItem> {
        let (dir_part, prefix) = match q.rfind('/') {
            Some(i) => (&q[..=i], &q[i + 1..]),
            None => ("", q),
        };
        let dir = if dir_part.starts_with('/') {
            PathBuf::from(dir_part)
        } else if let Some(rest) = dir_part.strip_prefix("~/") {
            dirs::home_dir().unwrap_or_default().join(rest)
        } else {
            self.base_dir().join(dir_part)
        };
        let entries = match self.dir_entries(&dir) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let lower = prefix.to_lowercase();
        let mut items: Vec<CompItem> = entries
            .iter()
            .filter(|(name, _)| {
                (prefix.starts_with('.') || !name.starts_with('.'))
                    && name.to_lowercase().starts_with(&lower)
            })
            .take(200)
            .map(|(name, is_dir)| CompItem {
                label: if *is_dir {
                    format!("{name}/")
                } else {
                    name.clone()
                },
                detail: String::new(),
                value: format!("@{dir_part}{name}{}", if *is_dir { "/" } else { "" }),
                dir: *is_dir,
            })
            .collect();
        items.truncate(50);
        if items.len() == 1 && items[0].value == format!("@{q}") && !items[0].dir {
            return Vec::new();
        }
        items
    }

    fn dir_entries(&mut self, dir: &Path) -> Option<Vec<(String, bool)>> {
        if let Some((at, e)) = self.dir_cache.get(dir)
            && at.elapsed() < Duration::from_secs(5)
        {
            return Some(e.clone());
        }
        match &self.backend {
            Backend::Local(_) => {
                let mut v: Vec<(String, bool)> = std::fs::read_dir(dir)
                    .ok()?
                    .filter_map(|e| e.ok())
                    .take(5000)
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().to_string(),
                            e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                        )
                    })
                    .collect();
                v.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                });
                self.dir_cache
                    .insert(dir.to_path_buf(), (Instant::now(), v.clone()));
                Some(v)
            }
            Backend::Remote { .. } => {
                if self.dir_pending.insert(dir.to_path_buf()) {
                    let d = dir.to_path_buf();
                    self.call::<Vec<DirEntry>, _>(
                        Request::ListDir {
                            path: dir.display().to_string(),
                        },
                        move |app, r| {
                            app.dir_pending.remove(&d);
                            if let Ok(list) = r {
                                let v = list.into_iter().map(|e| (e.name, e.is_dir)).collect();
                                app.dir_cache.insert(d, (Instant::now(), v));
                                app.update_completion();
                            }
                        },
                    );
                }
                self.dir_cache.get(dir).map(|(_, e)| e.clone())
            }
        }
    }

    // ---- popups ------------------------------------------------------------------------

    /// Close the popup; a list opened from the settings goes back there.
    pub(super) fn close_modal(&mut self) {
        let back = matches!(&self.modal, Some(Modal::Picker(p)) if p.from_settings);
        self.modal = None;
        if back {
            self.open_settings();
        }
    }

    fn modal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = self.pane().page.max(3);
        match self.modal.as_mut() {
            Some(Modal::Info(info)) => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::F(1) => {
                    self.modal = None
                }
                KeyCode::Up | KeyCode::Char('k') => info.scroll = info.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => info.scroll += 1,
                KeyCode::PageUp => info.scroll = info.scroll.saturating_sub(page),
                KeyCode::PageDown | KeyCode::Char(' ') => info.scroll += page,
                KeyCode::Home => info.scroll = 0,
                KeyCode::End => info.scroll = usize::MAX / 2,
                _ => {}
            },
            Some(Modal::Picker(p)) => {
                let n = p.visible().len();
                let filter = p.filterable();
                match k.code {
                    KeyCode::Esc if filter && !p.filter.is_empty() => {
                        p.filter.clear();
                        p.selected = 0;
                    }
                    KeyCode::Esc | KeyCode::F(2) => self.close_modal(),
                    KeyCode::Up => {
                        p.selected = if p.selected == 0 {
                            n.saturating_sub(1)
                        } else {
                            p.selected - 1
                        }
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        p.selected = if p.selected + 1 >= n {
                            0
                        } else {
                            p.selected + 1
                        }
                    }
                    KeyCode::Char('k') if !filter => p.selected = p.selected.saturating_sub(1),
                    KeyCode::Char('j') if !filter => {
                        p.selected = (p.selected + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::PageUp => p.selected = p.selected.saturating_sub(10),
                    KeyCode::PageDown => p.selected = (p.selected + 10).min(n.saturating_sub(1)),
                    KeyCode::Backspace if filter => {
                        p.filter.pop();
                        p.selected = 0;
                    }
                    KeyCode::Char('u') if ctrl && filter => {
                        p.filter.clear();
                        p.selected = 0;
                    }
                    KeyCode::Char(c @ '1'..='9') if !filter && !ctrl && !alt => {
                        let i = c as usize - '1' as usize;
                        if i < n {
                            p.selected = i;
                            self.pick();
                        }
                    }
                    KeyCode::Char(c) if filter && !ctrl && !alt => {
                        p.filter.push(c);
                        p.selected = 0;
                    }
                    KeyCode::Enter => self.pick(),
                    KeyCode::Char(' ') if !filter => self.pick(),
                    _ => {}
                }
            }
            Some(Modal::Rewind(f)) => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                KeyCode::Char('f') if f.file_changes() > 0 && !f.running => f.files = !f.files,
                KeyCode::Char('c') if !f.running => f.conversation = !f.conversation,
                KeyCode::Enter | KeyCode::Char('y') if !f.running => {
                    if !f.files && !f.conversation {
                        self.toast_warn("Choose the conversation, the files, or both (c / f)");
                    } else {
                        f.running = true;
                        self.rewind_run();
                    }
                }
                _ => {}
            },
            Some(Modal::Confirm(_)) => match k.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    if let Some(Modal::Confirm(c)) = self.modal.take() {
                        match c.action {
                            ConfirmAction::Permission(m) => self.set_permission(m),
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q') => self.modal = None,
                _ => {}
            },
            None => {}
        }
    }

    fn pick(&mut self) {
        let Some(Modal::Picker(p)) = self.modal.take() else {
            return;
        };
        let vis = p.visible();
        let Some(item) = vis.get(p.selected).and_then(|i| p.items.get(*i)) else {
            self.modal = Some(Modal::Picker(p));
            return;
        };
        if item.disabled {
            self.toast_warn(format!("{} isn't available: {}", item.label, item.badge));
            self.modal = Some(Modal::Picker(p));
            return;
        }
        let value = item.value.clone();
        let back = p.from_settings;
        match p.kind {
            PickKind::Chats => {
                if self.session_id().as_deref() != Some(value.as_str()) {
                    self.open_chat(value);
                }
            }
            PickKind::Model => match value.parse::<ModelRef>() {
                Ok(m) => self.set_model(m),
                Err(e) => self.error(e),
            },
            PickKind::DefaultModel => match value.parse::<ModelRef>() {
                Ok(m) => self.set_default_model(m),
                Err(e) => self.toast_warn(e),
            },
            PickKind::Thinking => {
                if let Some(t) = parse_thinking(&value) {
                    self.set_thinking(t);
                }
            }
            PickKind::Permission => {
                if let Some(m) = parse_permission(&value) {
                    self.choose_permission(m);
                }
            }
            PickKind::Theme => {
                if let Some(pref) = ThemePref::parse(&value) {
                    self.set_theme(pref);
                }
            }
            PickKind::Rewind => {
                if let Ok(seq) = value.parse::<u64>() {
                    self.rewind_preview(seq);
                }
            }
            PickKind::Settings => return self.settings_choose(&value, p.selected),
        }
        if back && self.modal.is_none() {
            self.open_settings();
        }
    }

    pub(super) fn open_info(&mut self, title: &str, lines: Vec<Line<'static>>) {
        self.modal = Some(Modal::Info(Info {
            title: title.into(),
            lines,
            scroll: 0,
        }));
    }

    /// `/help`: the keys that matter and the commands, on one screen.
    pub(super) fn open_help(&mut self) {
        let t = self.theme;
        let mut lines = Vec::new();
        for (k, d) in HELP_KEYS {
            lines.push(Line::from(vec![
                span(format!("{k:<16}"), t.key()),
                span(d.to_string(), t.muted()),
            ]));
        }
        lines.push(Line::default());
        let names: Vec<String> = commands::COMMANDS
            .iter()
            .map(|c| format!("/{}", c.name))
            .collect();
        lines.push(Line::from(vec![
            span(format!("{:<16}", "commands"), t.key()),
            span(names.join("  "), t.accent_text()),
        ]));
        lines.push(Line::from(vec![
            span(" ".repeat(16), Style::new()),
            span("type / to see what each one does", t.faint()),
        ]));
        lines.push(Line::default());
        lines.push(Line::from(span(
            "Your chats are in the Tessera apps too; bots, notes and the rest of Tessera live there.",
            t.faint(),
        )));
        self.open_info("Keys and commands", lines);
    }

    fn open_chats(&mut self) {
        let mut picker = Picker::new(PickKind::Chats, "Your chats");
        picker.empty = "No chats yet".into();
        self.modal = Some(Modal::Picker(picker));
        let current = self.session_id();
        let here = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.call::<Vec<SessionInfo>, _>(
            Request::ListSessions { archived: false },
            move |app, r| {
                let Some(Modal::Picker(p)) = app.modal.as_mut() else {
                    return;
                };
                if p.kind != PickKind::Chats {
                    return;
                }
                p.loading = false;
                match r {
                    Ok(mut list) => {
                        list.retain(|s| {
                            listed(s) && (s.last_seq > 0 || Some(&s.id) == current.as_ref())
                        });
                        list.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
                        p.items = list
                            .into_iter()
                            .map(|s| chat_item(&s, &here, &current))
                            .collect();
                        p.select_current();
                    }
                    Err(e) => app.error(e.message),
                }
            },
        );
    }

    /// The model a chat uses, from the providers we know.
    pub(super) fn model_info(&self, r: &ModelRef) -> Option<&ModelInfo> {
        self.providers
            .iter()
            .find(|p| p.id == r.provider)
            .and_then(|p| p.models.iter().find(|m| m.id == r.model))
    }

    /// Pick this chat's model, or (from the settings) the one new chats
    /// start with.
    pub(super) fn open_model_picker(&mut self, for_new_chats: bool) {
        let mut picker = Picker::new(
            if for_new_chats {
                PickKind::DefaultModel
            } else {
                PickKind::Model
            },
            if for_new_chats {
                "Model for new chats"
            } else {
                "Model"
            },
        );
        picker.from_settings = for_new_chats;
        picker.empty = "No models yet: run tessera setup to add one".into();
        self.modal = Some(Modal::Picker(picker));
        self.call::<Vec<ProviderInfo>, _>(
            Request::ListProviders { refresh: false },
            move |app, r| {
                let default = app.server.as_ref().and_then(|s| s.default_model.clone());
                let current = if for_new_chats {
                    default.clone()
                } else {
                    app.pane().session.as_ref().map(|s| s.model.clone())
                };
                if let Ok(p) = &r {
                    app.providers = p.clone();
                }
                let Some(Modal::Picker(p)) = app.modal.as_mut() else {
                    return;
                };
                if !matches!(p.kind, PickKind::Model | PickKind::DefaultModel) {
                    return;
                }
                p.loading = false;
                match r {
                    Ok(list) => {
                        for prov in list {
                            let usable = prov.authenticated || prov.auth == AuthMethod::None;
                            for m in &prov.models {
                                let r = ModelRef {
                                    provider: prov.id.clone(),
                                    model: m.id.clone(),
                                };
                                let mut badges = Vec::new();
                                if prov.offline {
                                    badges.push("offline");
                                }
                                if default.as_ref() == Some(&r) && !for_new_chats {
                                    badges.push("default");
                                }
                                if !usable {
                                    badges.push("sign in first");
                                }
                                let mut detail =
                                    format!("{} context", text::tokens(m.context_window));
                                if m.vision {
                                    detail.push_str(" · sees images");
                                }
                                let mut item = PickItem::new(
                                    if m.name.is_empty() || m.name == m.id {
                                        m.id.clone()
                                    } else {
                                        m.name.clone()
                                    },
                                    r.to_string(),
                                )
                                .detail(detail);
                                item.group = prov.name.clone();
                                item.current = current.as_ref() == Some(&r);
                                item.badge = badges.join(" · ");
                                item.disabled = !usable;
                                p.items.push(item);
                            }
                        }
                        p.select_current();
                    }
                    Err(e) => app.error(e.message),
                }
            },
        );
    }

    fn set_default_model(&mut self, m: ModelRef) {
        let shown = m.model.clone();
        self.call::<Value, _>(
            Request::SetDefaultModel { model: m },
            move |app, r| match r {
                Ok(_) => {
                    app.toast(format!("New chats use {shown}"));
                    app.refresh_providers();
                }
                Err(e) => app.toast_warn(format!("Couldn't set it: {}", e.message)),
            },
        );
    }

    pub(super) fn open_levels(&mut self, kind: PickKind, from_settings: bool) {
        let mut p = Picker::new(
            kind,
            if kind == PickKind::Thinking {
                "How hard the model thinks"
            } else {
                "Permissions"
            },
        );
        p.loading = false;
        p.from_settings = from_settings;
        let s = self.pane().session.as_ref();
        if kind == PickKind::Thinking {
            let opts = self.model_levels();
            if opts.is_empty() {
                let mut item =
                    PickItem::new("No thinking levels", "").detail("This model doesn't offer any");
                item.disabled = true;
                p.items.push(item);
            }
            for o in opts {
                let mut item =
                    PickItem::new(o.label.clone(), thinking_name(o.level)).detail(o.hint.clone());
                item.current = s.is_some_and(|s| s.thinking == o.level);
                p.items.push(item);
            }
        } else {
            let now = s
                .map(|s| s.permission)
                .or(self.new_permission)
                .unwrap_or(PermissionMode::Default);
            for (m, label, desc) in PERMS {
                let mut item = PickItem::new(label, permission_name(m)).detail(desc);
                item.current = m == now;
                p.items.push(item);
            }
        }
        p.select_current();
        self.modal = Some(Modal::Picker(p));
    }

    pub(super) fn open_themes(&mut self) {
        let title = match self.theme.depth() {
            Depth::None => "Theme · colours are off (NO_COLOR or --plain)",
            Depth::Ansi16 => "Theme · this terminal shows 16 colours",
            _ => "Theme",
        };
        let mut p = Picker::new(PickKind::Theme, title);
        p.loading = false;
        p.from_settings = true;
        let current = self.theme_saved.unwrap_or(self.theme.pref());
        for pref in ThemePref::ALL {
            let mut item = PickItem::new(pref.label(), pref.name()).detail(pref.about());
            item.current = pref == current;
            p.items.push(item);
        }
        p.select_current();
        self.modal = Some(Modal::Picker(p));
    }

    // ---- commands ----------------------------------------------------------------------

    /// Run a slash command as if typed.
    pub(super) fn run_command(&mut self, line: &str) {
        let (name, arg) = match line.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (line, ""),
        };
        let Some(cmd) = commands::find(name) else {
            return;
        };
        let t = self.theme;
        match cmd.name {
            "new" => self.new_chat_here(),
            "chats" => self.open_chats(),
            "split" => self.split(),
            "close" => self.close_pane(),
            "model" => match arg {
                "" => self.open_model_picker(false),
                _ => match arg.parse::<ModelRef>() {
                    Ok(m) => self.set_model(m),
                    Err(e) => self.error(format!("{e} (/model lists them)")),
                },
            },
            "thinking" => match arg {
                "" | "level" => self.open_levels(PickKind::Thinking, false),
                "show" => self.set_show_thinking(true),
                "hide" => self.set_show_thinking(false),
                _ => match parse_thinking(arg) {
                    Some(l) => self.set_thinking(l),
                    None => self.error(format!(
                        "/thinking takes a level (low, medium, high…), show or hide, not `{arg}`"
                    )),
                },
            },
            "compact" => {
                let Some(sid) = self.session_id() else { return };
                let pid = self.focus;
                let instructions = (!arg.is_empty()).then(|| arg.to_string());
                self.toast("Summarizing the conversation…");
                self.call::<Value, _>(
                    Request::Compact {
                        session_id: sid,
                        instructions,
                    },
                    move |app, r| {
                        if let Err(e) = r {
                            app.pane_error(pid, e.message);
                        }
                    },
                );
            }
            "rewind" => self.open_rewind(),
            "goal" => {
                let Some(sid) = self.session_id() else {
                    return self.toast_warn("The chat isn't ready yet");
                };
                if arg.is_empty() {
                    let goal = self
                        .pane()
                        .session
                        .as_ref()
                        .and_then(|s| s.task.as_ref())
                        .map(|t| t.goal.clone())
                        .filter(|g| !g.is_empty());
                    match goal {
                        Some(g) => self.open_info(
                            "Goal",
                            vec![
                                Line::from(span(g, t.text())),
                                Line::default(),
                                Line::from(span(
                                    "/goal <text> changes it · /goal clear removes it",
                                    t.faint(),
                                )),
                            ],
                        ),
                        None => self.toast(
                            "No goal yet: /goal <text> sets one, and the agent keeps it through every summary",
                        ),
                    }
                    return;
                }
                let goal = if arg == "clear" {
                    String::new()
                } else {
                    arg.to_string()
                };
                self.call::<SessionInfo, _>(
                    Request::SetGoal {
                        session_id: sid,
                        goal,
                    },
                    |app, r| match r {
                        Ok(info) => {
                            let set = info.task.as_ref().is_some_and(|t| !t.goal.is_empty());
                            app.toast(if set {
                                "Goal set: the agent has been told"
                            } else {
                                "Goal removed"
                            });
                        }
                        Err(e) => app.error(e.message),
                    },
                );
            }
            "copy" => match self.pane().transcript.last_assistant_text() {
                Some(txt) => {
                    osc52_copy(&txt);
                    self.toast(format!("Copied {} characters", txt.chars().count()));
                }
                None => self.toast_warn("Nothing to copy yet"),
            },
            "settings" => self.open_settings(),
            "help" => self.open_help(),
            "quit" => self.quit = true,
            _ => {}
        }
    }

    /// Keys for the queued messages above the composer: ↑ on an empty
    /// composer selects the last one; e edits, s steers, n sends now (the
    /// agent stops for it), x removes. Returns true when the key was used.
    fn queue_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        let p = self.pane();
        let n = p.queued.len();
        if n == 0 {
            if p.queue_sel.is_some() {
                self.pane_mut().queue_sel = None;
            }
            return false;
        }
        let Some(i) = p.queue_sel else {
            if k.code == KeyCode::Up
                && p.editor.is_empty()
                && p.editing_queued.is_none()
                && p.completion.is_none()
            {
                self.pane_mut().queue_sel = Some(n - 1);
                return true;
            }
            return false;
        };
        let i = i.min(n - 1);
        let sending = self.pane().queued[i].interrupt;
        match k.code {
            KeyCode::Up => self.pane_mut().queue_sel = Some(i.saturating_sub(1)),
            KeyCode::Char('e' | 's' | 'n' | 'x')
            | KeyCode::Enter
            | KeyCode::Delete
            | KeyCode::Backspace
                if sending =>
            {
                self.toast("It's on its way: the agent is stopping to read it");
            }
            KeyCode::Down => {
                self.pane_mut().queue_sel = (i + 1 < n).then_some(i + 1);
            }
            KeyCode::Esc => self.pane_mut().queue_sel = None,
            KeyCode::Char('e') | KeyCode::Enter => {
                let text_s = self.pane().queued[i].text.clone();
                let p = self.pane_mut();
                p.queue_sel = None;
                p.editing_queued = Some(i);
                p.editor.set_text(text_s);
                p.completion = None;
            }
            KeyCode::Char('s') => {
                if self.pane().queued[i].steer {
                    self.toast("It's already steering: the agent reads it at its next step");
                } else {
                    self.queue_request(i, QueueOp::Steer);
                }
            }
            KeyCode::Char('n') => {
                self.queue_request(i, QueueOp::SendNow);
                self.pane_mut().queue_sel = None;
            }
            KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
                self.queue_request(i, QueueOp::Remove);
                let left = n - 1;
                self.pane_mut().queue_sel = (left > 0).then_some(i.min(left - 1));
            }
            // Typing starts a new message.
            KeyCode::Char(_) => {
                self.pane_mut().queue_sel = None;
                return false;
            }
            _ => {}
        }
        true
    }

    fn queue_request(&mut self, index: usize, op: QueueOp) {
        let Some(sid) = self.session_id() else { return };
        let pid = self.focus;
        let index = index as u32;
        let (req, done) = match op {
            QueueOp::Steer => (
                Request::SteerQueued {
                    session_id: sid,
                    index,
                },
                "Steering: the agent reads it at its next step",
            ),
            QueueOp::SendNow => (
                Request::InterruptQueued {
                    session_id: sid,
                    index,
                },
                "Sent now: the agent stops and reads it",
            ),
            QueueOp::Remove => (
                Request::Dequeue {
                    session_id: sid,
                    index,
                },
                "Removed from the queue",
            ),
        };
        self.call::<Value, _>(req, move |app, r| match r {
            Ok(_) => app.toast(done),
            Err(e) if e.code == ErrorCode::NotFound => {
                app.toast_warn("That message was already delivered")
            }
            Err(e) => app.pane_error(pid, e.message),
        });
    }

    /// ⏎ while editing a queued message: save the new text.
    fn save_queued_edit(&mut self, index: usize, text_s: String) {
        let Some(sid) = self.session_id() else { return };
        if text_s.trim().is_empty() {
            self.toast_warn("The message is empty: x removes it from the queue instead");
            return;
        }
        let pid = self.focus;
        {
            let p = self.pane_mut();
            p.editing_queued = None;
            p.editor.clear();
            p.completion = None;
        }
        let restore = text_s.clone();
        self.call::<Value, _>(
            Request::EditQueued {
                session_id: sid,
                index: index as u32,
                text: text_s,
            },
            move |app, r| match r {
                Ok(_) => app.toast("Queued message updated"),
                Err(e) => {
                    if let Some(p) = app.pane_by_mut(pid)
                        && p.editor.is_empty()
                    {
                        p.editor.set_text(restore);
                    }
                    if e.code == ErrorCode::NotFound {
                        app.toast_warn(
                            "That message was already delivered: ⏎ sends this as a new one",
                        );
                    } else {
                        app.pane_error(pid, e.message);
                    }
                }
            },
        );
    }

    /// `/rewind`: pick one of your messages to go back to.
    fn open_rewind(&mut self) {
        let p = self.pane();
        let Some(s) = &p.session else {
            return self.toast_warn("The chat isn't ready yet");
        };
        if s.private {
            return self.toast_warn("Private chats can't be rewound");
        }
        if p.busy() {
            return self.toast_warn("Wait for the agent to finish (esc interrupts), then rewind");
        }
        let msgs = p.transcript.user_messages();
        let mut picker = Picker::new(PickKind::Rewind, "Rewind to before…");
        picker.loading = false;
        picker.empty = "None of your messages to go back to yet".into();
        for (i, (seq, ts, text)) in msgs.iter().enumerate().take(50) {
            picker.items.push(
                PickItem::new(tessera_core::util::one_line(text, 100), seq.to_string()).detail(
                    if i == 0 {
                        format!("latest · {}", ago(*ts))
                    } else {
                        ago(*ts)
                    },
                ),
            );
        }
        self.modal = Some(Modal::Picker(picker));
    }

    /// A dry run for rewinding to before message `seq`: what would change.
    fn rewind_preview(&mut self, seq: u64) {
        let Some(sid) = self.session_id() else { return };
        let text = self
            .pane()
            .transcript
            .user_messages()
            .into_iter()
            .find(|m| m.0 == seq)
            .map(|m| m.2)
            .unwrap_or_default();
        let req = Request::Rewind {
            session_id: sid.clone(),
            to_seq: seq.saturating_sub(1),
            files: true,
            conversation: true,
            dry_run: true,
        };
        self.call::<RewindResult, _>(req, move |app, r| match r {
            Ok(result) => {
                let files = !result.restored.is_empty() || !result.deleted.is_empty();
                app.modal = Some(Modal::Rewind(Box::new(RewindForm {
                    session_id: sid,
                    seq,
                    text,
                    result,
                    files,
                    conversation: true,
                    running: false,
                })));
            }
            Err(e) => app.toast_warn(format!("Can't rewind: {}", e.message)),
        });
    }

    /// Rewind for real, as chosen in the preview.
    fn rewind_run(&mut self) {
        let Some(Modal::Rewind(f)) = &self.modal else {
            return;
        };
        let (sid, seq, text, files, conversation) = (
            f.session_id.clone(),
            f.seq,
            f.text.clone(),
            f.files,
            f.conversation,
        );
        let req = Request::Rewind {
            session_id: sid.clone(),
            to_seq: seq.saturating_sub(1),
            files,
            conversation,
            dry_run: false,
        };
        self.call::<RewindResult, _>(req, move |app, r| {
            if matches!(app.modal, Some(Modal::Rewind(_))) {
                app.modal = None;
            }
            match r {
                Ok(res) => {
                    let mut parts = Vec::new();
                    if conversation {
                        // Your message comes back to edit and send again.
                        if let Some(p) = app
                            .panes
                            .iter_mut()
                            .find(|p| p.session_id() == Some(sid.as_str()))
                            && p.editor.is_empty()
                        {
                            p.editor.set_text(text);
                        }
                        parts.push(match &res.branch {
                            Some(b) => format!("the full chat is kept as “{}”", b.title),
                            None => "the conversation is back to before your message".into(),
                        });
                    }
                    if files {
                        let n = res.restored.len() + res.deleted.len();
                        parts.push(format!(
                            "{n} file{} put back",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    app.toast(format!("Rewound: {}", parts.join("; ")));
                }
                Err(e) => app.toast_warn(format!("Rewind failed: {}", e.message)),
            }
        });
    }
}

/// A row of `/chats`: its title, when, where (unless here) and what was
/// said last; a chat from the apps says so.
fn chat_item(s: &SessionInfo, here: &str, current: &Option<String>) -> PickItem {
    let status = match s.status {
        SessionStatus::Running | SessionStatus::Compacting => "working",
        SessionStatus::AwaitingApproval => "needs you",
        SessionStatus::Error => "error",
        _ => "",
    };
    let preview = text::sanitize(if tessera_core::tools::meta::is_check_ack(&s.preview) {
        ""
    } else {
        &s.preview
    })
    .replace('\n', " ")
    .replace("**", "")
    .replace('`', "");
    let mut detail = ago(s.updated_at);
    if s.cwd != here {
        detail.push_str(&format!(" · {}", compact_path(&s.cwd, 28)));
    }
    if !preview.is_empty() {
        detail.push_str(&format!(" · {preview}"));
    }
    let title = if s.title.trim().is_empty() {
        "Untitled".to_string()
    } else {
        s.title.clone()
    };
    let mut item = PickItem::new(title, s.id.clone()).detail(detail);
    item.current = Some(&s.id) == current.as_ref();
    item.badge = [status, if s.lean { "" } else { "from the apps" }]
        .iter()
        .filter(|x| !x.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
    item
}

/// Editing keys for the prompt. `single`: only one chat on screen, so
/// alt+←/→ still move by word.
pub(super) fn edit_key(
    e: &mut super::editor::Editor,
    k: KeyEvent,
    ctrl: bool,
    alt: bool,
    w: usize,
    single: bool,
) {
    match k.code {
        KeyCode::Char('j') if ctrl => e.insert_char('\n'),
        KeyCode::Char('a') if ctrl => e.home(),
        KeyCode::Char('e') if ctrl => e.end(),
        KeyCode::Char('b') if ctrl => e.left(),
        KeyCode::Char('f') if ctrl => e.right(),
        KeyCode::Char('w') if ctrl => e.delete_word_back(),
        KeyCode::Char('u') if ctrl => e.kill_line_back(),
        KeyCode::Char('k') if ctrl => e.kill_line_forward(),
        KeyCode::Char('d') if ctrl => e.delete(),
        KeyCode::Char('h') if ctrl => e.backspace(),
        KeyCode::Char('b') if alt && !ctrl => e.word_left(),
        KeyCode::Char('f') if alt && !ctrl => e.word_right(),
        KeyCode::Char('d') if alt && !ctrl => e.delete_word_forward(),
        // Plain characters; Ctrl+Alt is AltGr on some keyboards.
        KeyCode::Char(c) if ctrl == alt => e.insert_char(c),
        KeyCode::Backspace if ctrl || alt => e.delete_word_back(),
        KeyCode::Backspace => e.backspace(),
        KeyCode::Delete => e.delete(),
        KeyCode::Left if ctrl || (alt && single) => e.word_left(),
        KeyCode::Right if ctrl || (alt && single) => e.word_right(),
        KeyCode::Left => e.left(),
        KeyCode::Right => e.right(),
        KeyCode::Home => e.home(),
        KeyCode::End => e.end(),
        KeyCode::Up => {
            if e.at_first_row(w) && (!e.is_multiline() || e.browsing_history()) {
                e.history_prev();
            } else {
                e.move_vertical(w, false);
            }
        }
        KeyCode::Down => {
            if e.at_last_row(w) && e.browsing_history() {
                e.history_next();
            } else {
                e.move_vertical(w, true);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum QueueOp {
    Steer,
    SendNow,
    Remove,
}

fn is_command_line(t: &str) -> bool {
    let Some(first) = t.split_whitespace().next() else {
        return false;
    };
    let Some(name) = first.strip_prefix('/') else {
        return false;
    };
    // `/usr/bin/foo is broken` is a message, not a command.
    !name.is_empty() && !name.contains('/') && t.starts_with('/')
}

pub fn osc52_copy(s: &str) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    let mut out = std::io::stdout();
    let seq = format!("\x1b]52;c;{b64}\x07");
    if std::env::var_os("TMUX").is_some() {
        // Also pass through to the outer terminal.
        let _ = write!(out, "\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"));
    }
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// The keys `/help` lists.
pub const HELP_KEYS: &[(&str, &str)] = &[
    ("enter", "send · shift+enter or ctrl+j for a new line"),
    ("esc", "interrupt the agent · clear what you typed"),
    ("alt+enter", "while it works: steer it with what you typed"),
    (
        "↑ ↓",
        "your earlier messages (↑ on an empty line: queued ones)",
    ),
    ("@", "mention a file"),
    ("y  a  n", "allow · allow for this chat · deny"),
    ("shift+tab", "how hard the model thinks"),
    ("ctrl+o  ctrl+t", "show every step · show the thinking"),
    ("pgup  pgdn", "scroll · ctrl+end back to the bottom"),
    ("alt+← →", "move between chats side by side (/split)"),
    ("ctrl+c twice", "quit"),
];

/// Interface preferences remembered between runs (`tui.json`; older files
/// carried more, which is ignored).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Capture the mouse so the wheel scrolls the chat (Shift+drag selects text).
    pub mouse: bool,
    pub show_thinking: bool,
    /// A theme name: `auto`, `dark`, `light`, `contrast`, `gruvbox`…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Permission for new chats (`default`, `ask`, `yolo`, `readonly`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mouse: true,
            show_thinking: false,
            theme: None,
            permission: None,
        }
    }
}

impl Prefs {
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = tessera_core::config::write_atomic(path, json.as_bytes(), false);
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::tui::pane::tests::info;

    pub(crate) fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    pub(crate) fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn command_detection() {
        assert!(is_command_line("/model"));
        assert!(is_command_line("/goal ship it"));
        assert!(!is_command_line("/usr/bin/env is weird"));
        assert!(!is_command_line("hello /model"));
        assert!(!is_command_line("/"));
    }

    #[test]
    fn formatting() {
        assert_eq!(compact_path("/a/b/c/deep/project", 16), "…/c/deep/project");
        assert_eq!(compact_path("/a/b/c/deep/project", 15), "…/deep/project");
        assert_eq!(compact_path("/short", 16), "/short");
    }

    #[test]
    fn parse_levels_and_presets() {
        assert_eq!(parse_thinking("HIGH"), Some(ThinkingLevel::High));
        assert_eq!(
            parse_permission("read-only"),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(parse_permission("Ask me"), Some(PermissionMode::Ask));
        assert_eq!(parse_permission("yolo"), Some(PermissionMode::Auto));
        assert_eq!(parse_permission("nope"), None);
        for (m, label, _) in PERMS {
            assert_eq!(parse_permission(permission_name(m)), Some(m));
            assert_eq!(perm_label(m), label);
        }
    }

    #[test]
    fn the_most_recent_chat_in_this_folder_reopens() {
        let at = |id: &str, cwd: &str, seq: u64, updated: u64| {
            let mut s = info(id, SessionStatus::Idle);
            s.cwd = cwd.into();
            s.last_seq = seq;
            s.updated_at = updated;
            s
        };
        let mut bot = at("bot", "/work/x", 1, 50);
        bot.bot_id = Some("b1".into());
        let list = vec![
            at("a", "/work/x", 3, 10),
            at("b", "/work/x", 5, 20),
            // Never written in: not reopened.
            at("e", "/work/x", 0, 30),
            at("o", "/work/y", 1, 40),
            bot,
        ];
        assert_eq!(recent_chat(&list, "/work/x").as_deref(), Some("b"));
        assert_eq!(recent_chat(&list, "/work/z"), None);
    }

    pub async fn test_app() -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "tessera-tui-app-{}-{}",
            std::process::id(),
            tessera_core::util::now_ms() % 1_000_000 + rand_suffix()
        ));
        let engine = tessera_core::Engine::open(
            tessera_core::config::Paths::at(&dir),
            tessera_core::EngineOptions { scheduler: false },
        )
        .await
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(Backend::Local(engine), Theme::test(), false, tx);
        app.screen = Rect::new(0, 0, 200, 60);
        (app, dir)
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::SeqCst) * 7919
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slash_commands_complete_and_run() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(info("s1", SessionStatus::Idle));
        // "/" lists every command, in order; typing narrows it; tab takes it.
        typed(&mut app, "/");
        let c = app.pane().completion.as_ref().expect("the command list");
        assert_eq!(c.items.len(), commands::COMMANDS.len());
        assert_eq!(c.items[0].label, "/new");
        typed(&mut app, "ch");
        assert_eq!(
            app.pane().completion.as_ref().unwrap().items[0].label,
            "/chats"
        );
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.pane().editor.text(), "/chats ");
        app.pane_mut().editor.clear();
        // ⏎ on a bare command runs it.
        typed(&mut app, "/sett");
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Settings));
        app.on_key(key(KeyCode::Esc));
        assert!(app.modal.is_none());
        // /thinking completes its options.
        typed(&mut app, "/thinking s");
        assert_eq!(
            app.pane().completion.as_ref().unwrap().items[0].label,
            "show"
        );
        app.pane_mut().editor.clear();
        app.pane_mut().completion = None;
        // /help and ? open the help.
        app.run_command("/help");
        assert!(matches!(&app.modal, Some(Modal::Info(i)) if i.title == "Keys and commands"));
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('?')));
        assert!(matches!(&app.modal, Some(Modal::Info(_))));
        app.modal = None;
        // An unknown command says so and keeps what was typed.
        typed(&mut app, "/modle");
        app.pane_mut().completion = None;
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.pane().editor.text(), "/modle");
        assert!(!app.pane().transcript.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chats_split_side_by_side_and_close() {
        let (mut app, dir) = test_app().await;
        app.screen = Rect::new(0, 0, 200, 50);
        let first = app.focus;
        app.pane_mut().editor.set_text("draft one");
        app.run_command("/split");
        assert_eq!(app.panes.len(), 2);
        let second = app.focus;
        assert_ne!(first, second);
        // alt+← / alt+→ move between them; drafts stay with their chat.
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.focus, first);
        assert_eq!(app.pane().editor.text(), "draft one");
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(app.focus, second);
        // A narrow window has no room for more.
        app.screen = Rect::new(0, 0, 80, 24);
        app.run_command("/split");
        assert_eq!(app.panes.len(), 2);
        assert!(app.toast.is_some());
        // /close closes the focused one; the last one stays.
        app.run_command("/close");
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.focus, first);
        app.run_command("/close");
        assert_eq!(app.panes.len(), 1);
        // With one chat, alt+← moves by word in the composer.
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.pane().editor.cursor(), "draft ".len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settings_explain_yolo_remember_and_come_back() {
        let (mut app, dir) = test_app().await;
        let prefs = dir.join("tui.json");
        app.prefs_file = Some(prefs.clone());
        app.run_command("/settings");
        let Some(Modal::Picker(p)) = &app.modal else {
            panic!("settings")
        };
        let labels: Vec<&str> = p.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Permissions",
                "Model for new chats",
                "Show thinking",
                "Theme",
                "Mouse"
            ]
        );
        // Permissions → YOLO asks first; yes applies and remembers it.
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Permission));
        app.on_key(key(KeyCode::Char('3')));
        assert!(matches!(&app.modal, Some(Modal::Confirm(_))));
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(app.new_permission, Some(PermissionMode::Auto));
        assert_eq!(Prefs::load(&prefs).permission.as_deref(), Some("yolo"));
        // A toggle stays in the settings.
        app.run_command("/settings");
        app.on_key(key(KeyCode::Char('3')));
        assert!(app.show_thinking);
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Settings));
        assert!(Prefs::load(&prefs).show_thinking);
        // The theme list previews; esc puts the theme back and returns.
        app.on_terminal(TermEvent::Key(key(KeyCode::Char('4'))));
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Theme));
        app.on_terminal(TermEvent::Key(key(KeyCode::Down)));
        assert_ne!(app.theme.pref(), ThemePref::Dark);
        app.on_terminal(TermEvent::Key(key(KeyCode::Esc)));
        assert_eq!(app.theme.pref(), ThemePref::Dark);
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Settings));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rewritten_entries_replace_the_shown_ones() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(info("s1", SessionStatus::Idle));
        let user = |seq, text: &str| Entry {
            seq,
            ts: 0,
            body: EntryBody::User {
                content: vec![ContentPart::text(text)],
                author: None,
            },
        };
        app.on_event(Event::EntryAdded {
            session_id: "s1".into(),
            entry: user(1, "log in as ann / testpw2"),
        });
        assert_eq!(app.pane().editor.history(), ["log in as ann / testpw2"]);
        app.on_event(Event::EntriesChanged {
            session_id: "s1".into(),
            entries: vec![user(1, "log in as $LOGIN_USER / $LOGIN_PASSWORD")],
        });
        let shown = &app.pane().transcript.user_messages()[0].2;
        assert_eq!(shown, "log in as $LOGIN_USER / $LOGIN_PASSWORD");
        assert_eq!(app.pane().editor.history(), [shown.as_str()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_stored_without_its_secret_leaves_no_trace_in_history() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(info("s1", SessionStatus::Idle));
        let typed_s = "use key sk-proj-abc123 for the deploy";
        app.pane_mut().editor.push_history(typed_s);
        app.pane_mut().sent = Some(typed_s.into());
        app.proc_history.push(typed_s.into());
        app.on_event(Event::EntryAdded {
            session_id: "s1".into(),
            entry: Entry {
                seq: 1,
                ts: 0,
                body: EntryBody::User {
                    content: vec![ContentPart::text("use key $OPENAI_API_KEY for the deploy")],
                    author: None,
                },
            },
        });
        let stored = "use key $OPENAI_API_KEY for the deploy";
        assert_eq!(app.pane().editor.history(), [stored]);
        assert_eq!(app.proc_history, [stored]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rewinds_elsewhere_drop_the_rewound_entries() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(info("s1", SessionStatus::Idle));
        for seq in 1..=4 {
            app.pane_mut().transcript.add_entry(Entry {
                seq,
                ts: 0,
                body: EntryBody::User {
                    content: vec![ContentPart::text(format!("message {seq}"))],
                    author: None,
                },
            });
        }
        app.on_event(Event::SessionRewound {
            session_id: "s1".into(),
            to_seq: 2,
        });
        assert_eq!(app.pane().transcript.last_seq(), 2);
        app.run_command("/rewind");
        match &app.modal {
            Some(Modal::Picker(p)) => {
                assert_eq!(p.items.len(), 2);
                assert_eq!(p.items[0].label, "message 2");
                assert_eq!(p.items[0].value, "2");
            }
            _ => panic!("expected the rewind list"),
        }
        app.modal = None;
        app.pane_mut()
            .apply_info(info("s1", SessionStatus::Running));
        app.run_command("/rewind");
        assert!(app.modal.is_none(), "not while the agent works");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_messages_can_be_selected_edited_and_left() {
        let (mut app, dir) = test_app().await;
        app.pane_mut()
            .apply_info(info("s1", SessionStatus::Running));
        app.pane_mut().queued = vec![
            QueuedMessage {
                text: "first".into(),
                steer: false,
                interrupt: false,
                attachments: 0,
            },
            QueuedMessage {
                text: "second".into(),
                steer: true,
                interrupt: false,
                attachments: 0,
            },
        ];
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.pane().queue_sel, Some(1));
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.pane().queue_sel, Some(0));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.pane().queue_sel, None);
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Char('e')));
        assert_eq!(app.pane().editing_queued, Some(0));
        assert_eq!(app.pane().editor.text(), "first");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.pane().editing_queued, None);
        assert!(app.pane().editor.is_empty());
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(app.pane().queue_sel, None);
        assert_eq!(app.pane().editor.text(), "h");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approvals_are_answered_by_key_and_quitting_takes_two_ctrl_c() {
        let (mut app, dir) = test_app().await;
        app.pane_mut()
            .apply_info(info("s1", SessionStatus::AwaitingApproval));
        app.on_event(Event::ApprovalRequested {
            session_id: "s1".into(),
            request: ApprovalRequest {
                id: "r1".into(),
                call_id: "c".into(),
                tool: "bash".into(),
                summary: "rm -rf build".into(),
                reason: String::new(),
                arguments: Value::Null,
                once: false,
            },
        });
        assert_eq!(app.pane().approvals.len(), 1);
        // A "y" typed as the card appears (a word in progress) answers
        // nothing; typing doesn't reach the composer either.
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(app.pane().approvals.len(), 1);
        assert!(app.pane().editor.is_empty());
        app.pane_mut().approval_at = Some(Instant::now() - Duration::from_secs(1));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.pane().approvals.len(), 1);
        app.on_key(key(KeyCode::Char('n')));
        assert!(app.pane().approvals.is_empty());
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.text.starts_with("Denied"))
        );
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.quit && app.ctrl_c_armed());
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
