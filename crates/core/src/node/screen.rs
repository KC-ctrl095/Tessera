//! Desktop actions (desktop control): the screens, a screenshot, the mouse
//! and the keyboard of the signed-in desktop this machine runs in, for the
//! `desktop` tool. Windows only for now.
//!
//! Only that session, and only what Windows lets a program of the signed-in
//! user do: never the secure desktop (the sign-in and lock screen, Windows
//! security prompts, Ctrl+Alt+Delete) and never windows of programs that run
//! as administrator. Those are told apart before acting and said plainly;
//! nothing here tries to get around them.
//!
//! Coordinates are the screenshot's own pixels: a screen's picture fits
//! [`MAX_W`]×[`MAX_H`] and its scale depends only on the screen's size, so a
//! click at a picture's x/y lands where the picture shows it, without
//! remembering anything between actions.

// Elsewhere only the plain parts are used (and tested).
#![cfg_attr(not(windows), allow(dead_code))]

use tessera_proto::{DesktopOp, NodeImage, NodeResult};

/// This computer can do desktop actions.
pub const AVAILABLE: bool = cfg!(windows);

/// The largest screenshot, in pixels (models see more than this as no
/// better, and pay for every pixel).
pub const MAX_W: u32 = 1280;
pub const MAX_H: u32 = 800;

/// Longest text one `type` action types.
const MAX_TYPE: usize = 5_000;

/// A screen, in pixels of the whole desktop (the main one starts at 0,0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub main: bool,
}

impl Screen {
    /// How much its screenshots are shrunk (1: not at all).
    pub fn scale(&self) -> f64 {
        (f64::from(MAX_W) / f64::from(self.w.max(1)))
            .min(f64::from(MAX_H) / f64::from(self.h.max(1)))
            .min(1.0)
    }

    /// Its screenshot's size.
    pub fn picture(&self) -> (u32, u32) {
        let s = self.scale();
        (
            ((f64::from(self.w) * s).round() as u32).max(1),
            ((f64::from(self.h) * s).round() as u32).max(1),
        )
    }

    /// Where the screenshot's pixel `px`,`py` is on the desktop; an error
    /// for a pixel outside the picture.
    pub fn to_desktop(&self, n: usize, px: i32, py: i32) -> Result<(i32, i32), String> {
        let (pw, ph) = self.picture();
        if px < 0 || py < 0 || px >= pw as i32 || py >= ph as i32 {
            return Err(format!(
                "{px},{py} is outside screen {n}'s screenshot ({pw}×{ph}); take a screenshot and use its pixels"
            ));
        }
        let s = self.scale();
        let at = |p: i32, size: u32| {
            // The middle of the screen pixels this picture pixel stands for.
            (((f64::from(p) + 0.5) / s).floor() as i64).clamp(0, i64::from(size) - 1) as i32
        };
        Ok((self.x + at(px, self.w), self.y + at(py, self.h)))
    }
}

/// The screens in the order they are counted: the main one first, then
/// left to right, top to bottom.
pub fn ordered(mut screens: Vec<Screen>) -> Vec<Screen> {
    screens.sort_by_key(|s| (!s.main, s.x, s.y));
    screens
}

/// The `n`th screen (from 1).
fn pick(screens: &[Screen], n: u32) -> Result<(usize, Screen), String> {
    let i = n.max(1) as usize;
    screens.get(i - 1).map(|s| (i, *s)).ok_or_else(|| {
        format!(
            "there is no screen {i} (this computer has {} screen{})",
            screens.len(),
            if screens.len() == 1 { "" } else { "s" }
        )
    })
}

/// One key of a combination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    /// A Windows virtual-key code.
    Vk(u16),
    /// A character, found on the keyboard layout when it is pressed.
    Char(char),
}

/// Modifier keys, pressed before and let go after the rest.
const MODIFIERS: &[u16] = &[0x10, 0x11, 0x12, 0x5B];

/// Combinations (`ctrl+shift+esc`), several apart (`ctrl+a ctrl+c`).
pub fn parse_keys(text: &str) -> Result<Vec<Vec<Key>>, String> {
    let mut out = Vec::new();
    for combo in text.split_whitespace() {
        let mut keys = Vec::new();
        // A plus sign itself is "plus" (or the last part: `ctrl++`).
        let parts: Vec<&str> = if combo == "+" {
            vec!["+"]
        } else if let Some(head) = combo.strip_suffix("++") {
            head.split('+').chain(["+"]).collect()
        } else {
            combo.split('+').collect()
        };
        for p in parts {
            if p.is_empty() {
                return Err(format!(
                    "`{combo}` has an empty key; write keys like ctrl+c"
                ));
            }
            keys.push(key_of(p).ok_or_else(|| {
                format!("unknown key `{p}` (in `{combo}`); names like ctrl, alt, shift, win, enter, esc, tab, f5, left, pageup, or one character")
            })?);
        }
        let has = |vk: u16| keys.contains(&Key::Vk(vk));
        if has(0x11) && has(0x12) && (has(0x2E) || has(0x6E)) {
            return Err("Ctrl+Alt+Delete goes only to Windows itself (its secure screen); no program can send it".into());
        }
        if keys.len() > 6 {
            return Err(format!("`{combo}` has too many keys at once"));
        }
        out.push(keys);
    }
    if out.is_empty() {
        return Err("no keys given; e.g. ctrl+c or enter".into());
    }
    if out.len() > 20 {
        return Err("too many key combinations at once (at most 20)".into());
    }
    Ok(out)
}

fn key_of(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let vk = match lower.as_str() {
        "ctrl" | "control" | "ctl" => 0x11,
        "shift" => 0x10,
        "alt" | "option" | "menu" => 0x12,
        "win" | "windows" | "meta" | "super" | "cmd" | "command" | "start" => 0x5B,
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "tab" => 0x09,
        "space" | "spacebar" => 0x20,
        "backspace" | "back" | "bksp" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" | "page_up" => 0x21,
        "pagedown" | "pgdn" | "page_down" => 0x22,
        "left" | "arrowleft" => 0x25,
        "up" | "arrowup" => 0x26,
        "right" | "arrowright" => 0x27,
        "down" | "arrowdown" => 0x28,
        "printscreen" | "prtsc" | "print" => 0x2C,
        "capslock" => 0x14,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        "pause" => 0x13,
        "apps" | "contextmenu" => 0x5D,
        "volumeup" => 0xAF,
        "volumedown" => 0xAE,
        "mute" | "volumemute" => 0xAD,
        "plus" => return Some(Key::Char('+')),
        "minus" => return Some(Key::Char('-')),
        _ => 0,
    };
    if vk != 0 {
        return Some(Key::Vk(vk));
    }
    if let Some(n) = lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(Key::Vk(0x6F + n));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(Key::Vk(c.to_ascii_uppercase() as u16)),
        (Some(c), None) if c.is_ascii_digit() => Some(Key::Vk(c as u16)),
        (Some(c), None) if !c.is_control() => Some(Key::Char(c)),
        _ => None,
    }
}

/// Keys Windows wants marked "extended" (the ones beside the main block).
fn extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 | 0x2C | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x6F | 0x90
    )
}

/// Whether the desktop can be seen and used now, from what Windows says
/// about this program's session and desktops (kept apart from the calls
/// that ask, so it can be tested anywhere).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reach {
    Ok,
    /// Session 0: a Windows service, which has no desktop.
    Service,
    /// A window station nobody sees.
    Invisible,
    /// The session is disconnected (a remote desktop was closed): Windows
    /// draws nothing there and takes no input.
    Disconnected,
    /// The secure desktop has the screen: the sign-in or lock screen, a
    /// User Account Control prompt, Ctrl+Alt+Delete.
    Secure,
    /// This program runs on another desktop than the one on screen.
    Elsewhere,
}

/// `session`: this program's session id; `visible`: its window station
/// can be seen; `input`: the name of the desktop taking input (`None`:
/// Windows won't even open it for us, as with the secure desktop); `ours`:
/// the name of the desktop this program runs on.
pub fn reach(
    session: Option<u32>,
    visible: Option<bool>,
    disconnected: bool,
    input: Option<&str>,
    ours: &str,
) -> Reach {
    if session == Some(0) {
        return Reach::Service;
    }
    if visible == Some(false) {
        return Reach::Invisible;
    }
    if disconnected {
        return Reach::Disconnected;
    }
    match input {
        None => Reach::Secure,
        Some(name) if !name.eq_ignore_ascii_case("default") => Reach::Secure,
        Some(name) if !ours.eq_ignore_ascii_case(name) => Reach::Elsewhere,
        Some(_) => Reach::Ok,
    }
}

impl Reach {
    /// Why it can't, in plain words (`None`: it can).
    pub fn why(self) -> Option<&'static str> {
        Some(match self {
            Reach::Ok => return None,
            Reach::Service => {
                "this machine runs as a Windows service, which has no desktop. Desktop control works when it runs in the signed-in session (`tessera node service install`, or the desktop app's This computer)"
            }
            Reach::Invisible => {
                "this machine doesn't run on the visible desktop, so it can't see or use it"
            }
            Reach::Disconnected => {
                "the Windows session is disconnected (its remote desktop was closed), so Windows draws nothing and takes no input there. Ask the user to sign in to it again"
            }
            Reach::Secure => {
                "the screen is locked, or a Windows security prompt (sign-in, User Account Control, Ctrl+Alt+Delete) is showing: Tessera can't see or use that screen, by design. Ask the user to unlock it or answer the prompt themselves"
            }
            Reach::Elsewhere => {
                "this machine runs on another desktop than the one on screen, so it can't see or use it"
            }
        })
    }
}

/// Whether `program` is one of Tessera's own (the app, the machine, the
/// server): installed next to this one, or by its name. Agents never click
/// or type into those (approvals, settings and the Stop bar are the user's).
pub fn is_tessera_program(program: &std::path::Path, here: Option<&std::path::Path>) -> bool {
    // Windows paths, compared as Windows does (any case, either slash).
    let norm = |p: &std::path::Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    let program = norm(program);
    let (dir, name) = program.rsplit_once('\\').unwrap_or(("", program.as_str()));
    if let Some(h) = here.map(norm)
        && !dir.is_empty()
        && dir == h.trim_end_matches('\\')
    {
        return true;
    }
    matches!(
        name,
        "tessera.exe" | "tessera-app.exe" | "tessera-service.exe" | "tessera-desktop.exe"
    )
}

/// Whether input may go to a window of integrity level `theirs` from a
/// program at `ours` (Windows drops it without a word when it may not).
/// Unknown on either side: no (see [`may_act_as`] for ours).
pub fn may_send_to(ours: Option<u32>, theirs: Option<u32>) -> bool {
    match (ours, theirs) {
        (Some(a), Some(b)) => b <= a,
        // Windows won't even say: a program kept apart from us (run as
        // administrator, or protected); or ours unknown.
        _ => false,
    }
}

/// Windows' "high" integrity level: a program run as administrator.
pub const HIGH_INTEGRITY: u32 = 0x3000;

/// Desktop control only from a machine at the signed-in user's normal
/// level: one run as administrator could use administrators' windows (and
/// Windows' own prompts' programs) like its own; one whose level Windows
/// won't say, nothing.
pub fn may_act_as(ours: Option<u32>) -> Result<(), &'static str> {
    match ours {
        Some(level) if level < HIGH_INTEGRITY => Ok(()),
        Some(_) => Err(
            "Tessera runs as administrator on this computer, so its desktop control is off: it would reach administrators' windows too. Start Tessera there normally (not \"Run as administrator\"), then try again",
        ),
        None => Err(
            "Windows didn't say at which level Tessera runs on this computer, so its desktop control is off",
        ),
    }
}

/// Marks the input Tessera's desktop actions send (`dwExtraInfo`): the
/// Stop bar's guard refuses it where it arrives on Tessera's own windows,
/// and anywhere while it must not land (see [`input_refused`]).
pub const INPUT_MARK: usize = 0x5445_5353;

/// Marks the guard's own test input (see `stopbar::guard_alive`): never
/// delivered while the guard works, and harmless if it isn't.
pub const PROBE_MARK: usize = 0x5445_5350;

/// Desktop actions asked to stop that haven't ended yet (counted until they
/// have): whatever they still send is refused where it arrives.
static FENCED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A desktop action asked to stop, from now until it has ended (dropped
/// then).
pub fn fence() -> Fence {
    FENCED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Fence
}

pub struct Fence;

impl Drop for Fence {
    fn drop(&mut self) {
        FENCED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Input Tessera's desktop actions send must not land now: the user
/// stopped everything here, or an action asked to stop is still going.
/// Presses and turns are refused where they arrive (releases always pass).
pub fn input_refused() -> bool {
    HALT.halted() || FENCED.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// Where desktop coordinate `v` lies between `lo` and `hi` (the whole
/// desktop, whose left or top edge is below 0 when a screen sits left of
/// or above the main one), as Windows' absolute pointer positions count
/// it: 0 to 65535.
pub fn absolute(v: i32, lo: i32, hi: i32) -> i32 {
    let span = f64::from((hi - lo).max(1));
    (((f64::from(v - lo) + 0.5) * 65536.0 / span).floor() as i32).clamp(0, 65535)
}

/// One combination's key events, `(key, up)`: modifiers down first, then
/// the rest in order, then all up in reverse.
pub fn combo_events(keys: &[u16]) -> Vec<(u16, bool)> {
    let mut down: Vec<u16> = Vec::new();
    for k in keys {
        if !down.contains(k) {
            down.push(*k);
        }
    }
    down.sort_by_key(|vk| !MODIFIERS.contains(vk));
    let mut out: Vec<(u16, bool)> = down.iter().map(|k| (*k, false)).collect();
    out.extend(down.iter().rev().map(|k| (*k, true)));
    out
}

/// The keys still held down after only the first `sent` of `events` went
/// through, newest first: let go of them, or they stay pressed.
pub fn still_down(events: &[(u16, bool)], sent: usize) -> Vec<u16> {
    let mut held: Vec<u16> = Vec::new();
    for (k, up) in events.iter().take(sent) {
        if *up {
            held.retain(|h| h != k);
        } else if !held.contains(k) {
            held.push(*k);
        }
    }
    held.reverse();
    held
}

/// The user's Stop on this computer (the Stop bar, its key, `tessera node
/// stop`): set the moment they press it, before anything else is told.
pub struct Halt {
    on: std::sync::atomic::AtomicBool,
    /// How many times they did: an action begun before a Stop stays
    /// stopped even once they resume.
    count: std::sync::atomic::AtomicU64,
}

impl Halt {
    pub const fn new() -> Self {
        Self {
            on: std::sync::atomic::AtomicBool::new(false),
            count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn halt(&self) {
        // On first, then counted: an action that reads the count before
        // the Stop and "not on" after it can't exist (see `Stop::under`).
        self.on.store(true, std::sync::atomic::Ordering::SeqCst);
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn resume(&self) {
        self.on.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn halted(&self) -> bool {
        self.on.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn count(&self) -> u64 {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for Halt {
    fn default() -> Self {
        Self::new()
    }
}

static HALT: Halt = Halt::new();

/// The user stopped everything here: every desktop action ends at its next
/// step, and none starts until [`resume`].
pub fn halt() {
    HALT.halt();
}

/// The user resumed here (only new actions go ahead).
pub fn resume() {
    HALT.resume();
}

pub fn halted() -> bool {
    HALT.halted()
}

/// Asked to stop: the chat stopped it, its time ran out, the machine was
/// paused, turned off or narrowed, or the user pressed Stop here. A desktop
/// action looks before each step (each click, key combination, batch of
/// typing, drag step and the screenshot afterwards), lets go of what it
/// holds, and ends.
#[derive(Clone)]
pub struct Stop {
    asked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    halt: &'static Halt,
    /// The user's Stops so far when this began.
    halts: u64,
}

impl Default for Stop {
    fn default() -> Self {
        Self::under(&HALT)
    }
}

impl Stop {
    /// One that the user's Stop `halt` ends too.
    pub fn under(halt: &'static Halt) -> Self {
        let count = halt.count();
        Self {
            asked: Default::default(),
            halt,
            // Begun while stopped: stopped for good, even after a resume.
            halts: if halt.halted() { u64::MAX } else { count },
        }
    }

    pub fn stop(&self) {
        self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn stopped(&self) -> bool {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
            || self.halt.halted()
            || self.halt.count() != self.halts
    }

    /// Wait `d`, in short steps; false when asked to stop meanwhile.
    pub fn sleep(&self, d: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + d;
        while std::time::Instant::now() < until {
            if self.stopped() {
                return false;
            }
            std::thread::sleep(
                until
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(20)),
            );
        }
        !self.stopped()
    }
}

/// How long a chat keeps the screen after its last desktop action.
pub const LEASE: std::time::Duration = std::time::Duration::from_secs(60);

/// Which chat is using the screen with the mouse and keyboard: one at a
/// time, so one chat's clicks and keys never land between another's (a
/// click to focus a field, then the typing). Looking (screenshots) takes no
/// turn. The screen is free again a minute after the last action ended,
/// never while one still runs or waits.
#[derive(Default)]
pub struct Lease {
    holder: Option<String>,
    /// Its actions running or waiting now.
    active: usize,
    last: Option<std::time::Instant>,
}

impl Lease {
    /// Start an action for `chat`; how long until the screen is free when
    /// another chat has it.
    pub fn begin(
        &mut self,
        chat: &str,
        now: std::time::Instant,
    ) -> Result<(), std::time::Duration> {
        if let Some(holder) = &self.holder
            && holder != chat
        {
            if self.active > 0 {
                return Err(LEASE);
            }
            let since = self
                .last
                .map_or(LEASE, |l| now.saturating_duration_since(l));
            if since < LEASE {
                return Err(LEASE - since);
            }
            self.active = 0;
        }
        self.holder = Some(chat.to_string());
        self.active += 1;
        self.last = Some(now);
        Ok(())
    }

    /// One of `chat`'s actions ended: its minute counts from now.
    pub fn end(&mut self, chat: &str, now: std::time::Instant) {
        if self.holder.as_deref() == Some(chat) {
            self.active = self.active.saturating_sub(1);
            self.last = Some(now);
        }
    }
}

/// Input that Windows wouldn't let go of (a key or mouse button whose
/// release it refused): released before anything else is done, and
/// nothing else is done until it is.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Held {
    Key(u16),
    /// A typed character (a UTF-16 unit sent as such).
    Char(u16),
    /// The mouse button's release flag (e.g. `MOUSEEVENTF_LEFTUP`).
    Button(u32),
}

/// One key event of typing: Enter or Tab by key, anything else as the
/// character itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypeKey {
    Vk(u16),
    Unit(u16),
}

/// The key events typing `text` takes, in press/release pairs, each with
/// how many characters of `text` are complete once it went through (a
/// character outside the basic plane is two units; CR LF is one Enter).
pub fn type_plan(text: &str) -> Vec<(TypeKey, bool, usize)> {
    let mut out = Vec::new();
    let mut count = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let (keys, n): (Vec<TypeKey>, usize) = match c {
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                (vec![TypeKey::Vk(0x0D)], 2)
            }
            '\r' | '\n' => (vec![TypeKey::Vk(0x0D)], 1),
            '\t' => (vec![TypeKey::Vk(0x09)], 1),
            c => {
                let mut units = [0u16; 2];
                (
                    c.encode_utf16(&mut units)
                        .iter()
                        .map(|u| TypeKey::Unit(*u))
                        .collect(),
                    1,
                )
            }
        };
        let last = keys.len() - 1;
        for (i, k) in keys.iter().enumerate() {
            out.push((*k, false, count));
            out.push((*k, true, if i == last { count + n } else { count }));
        }
        count += n;
    }
    out
}

/// `plan` in batches of at most `max` events (at least one character's,
/// i.e. 4) that end only between characters: a stop between two batches
/// never leaves half a character (one outside the basic plane) typed.
pub fn type_batches(plan: &[(TypeKey, bool, usize)], max: usize) -> Vec<std::ops::Range<usize>> {
    let max = max.max(4);
    let mut out = Vec::new();
    let (mut start, mut cut) = (0, 0);
    for (i, e) in plan.iter().enumerate() {
        // The release that completes a character counts one more.
        let ends = e.1 && i > 0 && e.2 > plan[i - 1].2;
        if i + 1 - start > max {
            out.push(start..cut);
            start = cut;
        }
        if ends {
            cut = i + 1;
        }
    }
    if start < plan.len() {
        out.push(start..plan.len());
    }
    out
}

/// How many characters are surely typed once the first `took` events of
/// `plan` went through.
pub fn typed_chars(plan: &[(TypeKey, bool, usize)], took: usize) -> usize {
    took.checked_sub(1)
        .and_then(|i| plan.get(i))
        .map_or(0, |e| e.2)
}

/// What is left held after only the first `took` of the `releases` sent
/// went through.
pub fn left_held(releases: &[Held], took: usize) -> Vec<Held> {
    releases.iter().skip(took).copied().collect()
}

/// A screenshot as PNG, from rows of blue-green-red-unused bytes.
pub fn png_of(bgra: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let (pixels, _) = bgra.as_chunks::<4>();
    for [b, g, r, _] in pixels.iter().take((w * h) as usize) {
        rgb.extend_from_slice(&[*r, *g, *b]);
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

fn done(text: String, image: Option<NodeImage>) -> NodeResult {
    NodeResult {
        output: text,
        ok: true,
        refused: false,
        exit_code: None,
        image,
    }
}

fn failed(why: String) -> NodeResult {
    NodeResult {
        output: why,
        ok: false,
        refused: false,
        exit_code: None,
        image: None,
    }
}

/// Windows keeps it out of reach (the secure desktop, an administrator's
/// window, no desktop at all): refused, nothing happened.
fn out_of_reach(why: String) -> NodeResult {
    NodeResult {
        output: why,
        ok: false,
        refused: true,
        exit_code: None,
        image: None,
    }
}

/// Do `op` (`look`: a screenshot of the same screen afterwards). One at a
/// time: clicks and keys from two chats never interleave.
pub fn act(op: &DesktopOp, look: bool, stop: &Stop) -> NodeResult {
    // Without the Stop bar and its key nothing goes ahead: the user must
    // always be able to see and stop it.
    if let Err(why) = super::stopbar::ready() {
        return out_of_reach(why);
    }
    // What an earlier action held is still being let go after a Stop.
    if op.acts() && letting_go() {
        return out_of_reach(LETTING_GO_WHY.into());
    }
    // One that didn't stop when asked may still be acting: nothing else
    // until it has ended (and let go of what it held).
    if unresolved() {
        return out_of_reach(UNRESOLVED_WHY.into());
    }
    // Waiting for another one ends when this one is asked to stop.
    let _one = loop {
        match ONE.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if !stop.sleep(std::time::Duration::from_millis(20)) {
                    return failed(STOPPED.into());
                }
            }
        }
    };
    if stop.stopped() {
        return failed(STOPPED.into());
    }
    // After waiting: a Stop's letting go may have begun meanwhile.
    if op.acts() && letting_go() {
        return out_of_reach(LETTING_GO_WHY.into());
    }
    // After waiting: the bar may have gone meanwhile.
    if let Err(why) = super::stopbar::ready() {
        return out_of_reach(why);
    }
    if unresolved() {
        return out_of_reach(UNRESOLVED_WHY.into());
    }
    // Input only while the guard of Tessera's own windows sees it.
    if op.acts()
        && let Err(why) = super::stopbar::guard_alive()
    {
        return out_of_reach(why);
    }
    imp::act(op, look, stop)
}

/// Desktop actions asked to stop that haven't ended yet (a Windows call that
/// doesn't return): while there is one, no desktop action runs here. Only
/// its end clears it; one that never ends keeps the screen out of agents'
/// reach until Tessera restarts on this computer.
static UNRESOLVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const UNRESOLVED_WHY: &str = "an earlier desktop action here didn't stop when asked and may still be acting, so no desktop action runs on this computer until it has ended and let go of what it held (restarting Tessera there ends it)";

fn unresolved() -> bool {
    UNRESOLVED.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// A desktop action that didn't stop in time, counted from now until it
/// ends (dropped then): see [`UNRESOLVED`].
pub fn quarantine() -> Unresolved {
    UNRESOLVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Unresolved
}

pub struct Unresolved;

impl Drop for Unresolved {
    fn drop(&mut self) {
        UNRESOLVED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One desktop action at a time (see [`act`]).
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// After the user's Stop: once the action under way let go (it stops at its
/// next step), release whatever Windows still holds down from it.
/// Letting go after a Stop is under way (how many): no new input until done.
static LETTING_GO: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A Stop's letting go, counted from now (before a Resume can let new input
/// in) until [`let_go`] has finished with it.
pub fn begin_let_go() -> LettingGo {
    LETTING_GO.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    LettingGo
}

fn letting_go() -> bool {
    LETTING_GO.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// A drag's button, at once (from any thread: the Stop bar's too).
pub fn release_live() {
    imp::release_live();
}

/// Modifier keys and mouse buttons left down by an earlier run (Windows),
/// let go when the machine starts.
pub fn release_stuck() {
    imp::release_stuck();
}

/// Every modifier key and mouse button Windows says is down, let go (the
/// guard runs this in the user's session right after ending a machine that
/// may have held some; the user's own are let go too, once). False when
/// Windows refused some of it.
pub fn release_down() -> bool {
    imp::release_down()
}

pub fn let_go(_done: LettingGo) {
    // A drag's button first, at once: the action may be stuck in a call.
    imp::release_live();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match ONE.try_lock() {
            Ok(_one) => return imp::release_now(),
            Err(std::sync::TryLockError::Poisoned(p)) => {
                let _one = p.into_inner();
                return imp::release_now();
            }
            Err(std::sync::TryLockError::WouldBlock) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // Still busy: release anyway (its own release comes after ours).
            Err(std::sync::TryLockError::WouldBlock) => return imp::release_now(),
        }
    }
}

pub struct LettingGo;

impl Drop for LettingGo {
    fn drop(&mut self) {
        LETTING_GO.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

const STOPPED: &str = "stopped before it finished (what was done until then stays done)";

const LETTING_GO_WHY: &str =
    "keys and buttons held before the user's Stop are still being let go; try again in a moment";

/// Whether window `w` is one of Tessera's own (Windows).
#[cfg(windows)]
pub fn tessera_window(w: windows_sys::Win32::Foundation::HWND) -> bool {
    imp::tessera_window(w)
}

/// The desktop desktop control reaches here, for `tessera node status`
/// and the app (e.g. "Windows desktop, 2 screens"); `None` without one.
pub fn session_label() -> Option<String> {
    imp::session_label()
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn session_label() -> Option<String> {
        None
    }

    pub fn release_now() {}

    pub fn release_live() {}

    pub fn release_stuck() {}

    pub fn release_down() -> bool {
        true
    }

    pub fn act(_op: &DesktopOp, _look: bool, _stop: &Stop) -> NodeResult {
        out_of_reach("desktop actions aren't available on this computer yet; with desktop control on, commands get the desktop session and can use its own tools".into())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use base64::Engine as _;
    use std::time::Duration;
    use tessera_proto::MouseButton;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetDC,
        GetMonitorInfoW, HALFTONE, HDC, HMONITOR, MONITORINFO, ReleaseDC, SRCCOPY, SelectObject,
        SetBrushOrgEx, SetStretchBltMode, StretchBlt,
    };
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSConnectState,
        WTSDisconnected, WTSFreeMemory, WTSQuerySessionInformationW,
    };
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop,
        GetUserObjectInformationW, OpenInputDesktop, UOI_FLAGS, UOI_NAME, USEROBJECTFLAGS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT,
        MapVirtualKeyW, SendInput, VkKeyScanW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GA_ROOT, GetAncestor, GetCursorInfo,
        GetCursorPos, GetForegroundWindow, GetIconInfo, GetSystemMetrics, GetWindowThreadProcessId,
        ICONINFO, SM_CXCURSOR, SM_CYCURSOR, SetCursorPos, WindowFromPoint,
    };

    /// The window station can be seen (`USEROBJECTFLAGS::dwFlags`).
    const WSF_VISIBLE: u32 = 1;
    const WHEEL_DELTA: i32 = 120;

    const ADMIN: &str = "that window belongs to a program running as administrator: Windows doesn't let programs of the signed-in user (Tessera included) click or type into it. Ask the user to do this step, or to open that program without administrator rights";

    const TESSERA_WINDOW: &str = "that is one of Tessera's own windows (its Stop bar or the app): approvals, settings and Stop there are the user's, so agents can't click or type into them";

    /// Per-monitor DPI awareness for this thread while it lives, so every
    /// position and size is in real pixels.
    struct Dpi(DPI_AWARENESS_CONTEXT);

    impl Dpi {
        fn aware() -> Self {
            // SAFETY: plain call; the previous context is put back on drop.
            Self(unsafe {
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            })
        }
    }

    impl Drop for Dpi {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the context this thread had before.
                unsafe { SetThreadDpiAwarenessContext(self.0) };
            }
        }
    }

    pub fn session_label() -> Option<String> {
        let _dpi = Dpi::aware();
        match reachable() {
            Reach::Ok => {}
            Reach::Secure => return Some("Windows desktop (locked right now)".into()),
            Reach::Disconnected => return Some("Windows desktop (disconnected right now)".into()),
            _ => return None,
        }
        let n = screens().map(|s| s.len()).unwrap_or(0);
        Some(match n {
            0 => "Windows desktop".into(),
            1 => "Windows desktop, 1 screen".into(),
            n => format!("Windows desktop, {n} screens"),
        })
    }

    /// What an earlier run wrote down as held (a drag's button, releases
    /// Windows refused) when the machine starts: let go of exactly that
    /// (never what the user holds); what Windows still refuses stays noted,
    /// and desktop actions wait for it (see `release_held`).
    pub fn release_stuck() {
        let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Only Tessera's own records (by name, before anything is read):
            // one per machine process, or the single file of test builds
            // before that.
            let legacy = name == "tessera-held-input.json";
            if !legacy && !(name.starts_with("tessera-held-input-") && name.ends_with(".json")) {
                continue;
            }
            let Some(text) = read_small(&entry.path()) else {
                tracing::warn!("a held-input record ({name}) can't be read; left as it is");
                continue;
            };
            // Records name their machine process; the earliest test builds
            // wrote only what was held.
            let (pid, held) = match serde_json::from_str::<HeldRecord>(&text) {
                Ok(r) => (Some(r.pid), r.held),
                Err(_) => match serde_json::from_str::<Vec<Held>>(&text) {
                    Ok(held) if legacy => (None, held),
                    _ => {
                        tracing::warn!(
                            "a held-input record ({name}) isn't understood; left as it is"
                        );
                        continue;
                    }
                },
            };
            // A machine process that still runs holds its own.
            if pid.is_some_and(|pid| {
                pid == std::process::id() || crate::platform::windows::alive(pid)
            }) {
                continue;
            }
            note_held(&held);
            if release_held().is_ok() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// A held-input record's text, if it is one's size (a few hundred
    /// bytes): never a large file read whole.
    fn read_small(path: &std::path::Path) -> Option<String> {
        use std::io::Read as _;
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .ok()?;
        (text.len() <= 64 * 1024).then_some(text)
    }

    pub fn release_down() -> bool {
        // Windows says what is down only on the desktop taking input; on
        // another one (locked, say) nothing would look held: not let go.
        if !matches!(reachable(), Reach::Ok) {
            return false;
        }
        // Left and right Shift, Ctrl, Alt and Windows keys.
        const KEYS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];
        // SAFETY: plain calls.
        let down = |vk: i32| unsafe { GetAsyncKeyState(vk) } < 0;
        let mut ups: Vec<INPUT> = KEYS
            .iter()
            .filter(|vk| down(i32::from(**vk)))
            .map(|vk| vk_event(*vk, true))
            .collect();
        for (vk, up) in [
            (0x01, MOUSEEVENTF_LEFTUP),
            (0x02, MOUSEEVENTF_RIGHTUP),
            (0x04, MOUSEEVENTF_MIDDLEUP),
        ] {
            if down(vk) {
                ups.push(mouse(0, 0, 0, up));
            }
        }
        let sent = ups.is_empty() || send_raw(&ups) == ups.len();
        // The desktop may have changed while it looked (locked, a Windows
        // prompt): then what it saw counts for nothing.
        sent && matches!(reachable(), Reach::Ok)
    }

    /// What Windows still holds down from desktop actions, let go now (what
    /// it refuses stays noted for the next action).
    pub fn release_now() {
        let _ = release_held();
    }

    /// What a desktop action holds down right now (a drag's button).
    static LIVE: std::sync::Mutex<Vec<Held>> = std::sync::Mutex::new(Vec::new());

    /// Note `h` as held (`down`) or not. For a button held across calls
    /// (a drag) whether it was written down too (see [`write_held`]).
    fn hold(h: Held, down: bool) -> bool {
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|x| *x != h);
        if down {
            live.push(h);
        }
        drop(live);
        // A button held across calls (a drag) is written down: if this
        // process is ended meanwhile, the next start lets go of it. (Keys
        // go down and up within one call, which Windows takes whole.)
        !matches!(h, Held::Button(_)) || write_held()
    }

    /// Where what Tessera holds down across calls is written (this user's
    /// temporary folder: it outlives this process, not a restart of the
    /// computer, which lets go of everything anyway).
    fn held_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera-held-input-{}.json", std::process::id()))
    }

    /// The record: which machine process holds what (another one running
    /// at the same time, for another Tessera folder, has its own and
    /// leaves this alone).
    #[derive(serde::Serialize, serde::Deserialize)]
    struct HeldRecord {
        pid: u32,
        held: Vec<Held>,
    }

    /// Write down a drag's held button and what Windows wouldn't let go of;
    /// nothing held, no record.
    fn write_held() -> bool {
        let mut all: Vec<Held> = LIVE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|h| matches!(h, Held::Button(_)))
            .copied()
            .collect();
        for h in HELD.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !all.contains(h) {
                all.push(*h);
            }
        }
        // This process's own record (another machine running at the same
        // time has its own).
        let file = held_file();
        if all.is_empty() {
            let _ = std::fs::remove_file(&file);
            return true;
        }
        let record = HeldRecord {
            pid: std::process::id(),
            held: all,
        };
        serde_json::to_string(&record)
            .ok()
            .is_some_and(|text| crate::config::write_atomic(&file, text.as_bytes(), true).is_ok())
    }

    /// The user's Stop: let go of what an action holds right now, without
    /// waiting for it (a second release later does nothing); what Windows
    /// refuses is kept for the next action.
    pub fn release_live() {
        let live: Vec<Held> = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
        if live.is_empty() {
            return;
        }
        let releases: Vec<INPUT> = live.iter().map(|h| release_of(*h)).collect();
        let took = send_raw(&releases);
        note_held(&left_held(&live, took));
    }

    pub fn act(op: &DesktopOp, look: bool, stop: &Stop) -> NodeResult {
        let _dpi = Dpi::aware();
        // SAFETY: plain call on this process's own handle.
        if let Err(why) = may_act_as(unsafe { integrity(GetCurrentProcess()) }) {
            return out_of_reach(why.into());
        }
        if let Err(why) = out_of_reach_now() {
            return out_of_reach(why);
        }
        // Keys or buttons an earlier action couldn't let go of: first.
        if op.acts()
            && let Err(why) = release_held()
        {
            return out_of_reach(why);
        }
        let screens = match screens() {
            Ok(s) => s,
            Err(why) => return failed(why),
        };
        let result = match op {
            DesktopOp::Screens => Ok(done(describe_screens(&screens), None)),
            DesktopOp::Screenshot { .. } => step(stop, None)
                .and_then(|()| shot(&screens, op.screen()))
                .map(|(t, i)| done(t, Some(i))),
            _ => input(op, &screens, stop),
        };
        let mut r = match result {
            Ok(r) => r,
            Err(Why::Refused(why)) => return out_of_reach(why),
            Err(Why::Failed(why)) => return failed(why),
            Err(Why::Stopped) => return failed(STOPPED.into()),
        };
        if look && r.ok {
            // Let the screen catch up with what was done; then look again
            // whether it may be seen (it may have been locked meanwhile).
            let seen = if !stop.sleep(Duration::from_millis(500)) {
                Err(STOPPED.to_string())
            } else {
                out_of_reach_now().and_then(|()| match shot(&screens, op.screen()) {
                    Ok(x) => Ok(x),
                    Err(Why::Refused(w) | Why::Failed(w)) => Err(w),
                    Err(Why::Stopped) => Err(STOPPED.into()),
                })
            };
            match seen {
                Ok((t, i)) => {
                    r.output.push('\n');
                    r.output.push_str(&t);
                    r.image = Some(i);
                }
                Err(w) => r
                    .output
                    .push_str(&format!("\n(No screenshot afterwards: {w}.)")),
            }
        }
        r
    }

    enum Why {
        Refused(String),
        Failed(String),
        Stopped,
    }

    impl From<String> for Why {
        fn from(s: String) -> Self {
            Why::Failed(s)
        }
    }

    /// Whether this machine runs in a signed-in, connected desktop session
    /// whose desktop is the one taking input now (not the secure one).
    fn reachable() -> Reach {
        // SAFETY: plain calls with valid out-pointers; what Windows hands
        // over is freed or closed here.
        unsafe {
            let mut id = 0u32;
            let session = (ProcessIdToSessionId(GetCurrentProcessId(), &mut id) != 0).then_some(id);
            let mut flags: USEROBJECTFLAGS = std::mem::zeroed();
            let station = GetProcessWindowStation();
            let visible = (!station.is_null()
                && GetUserObjectInformationW(
                    station,
                    UOI_FLAGS,
                    (&mut flags as *mut USEROBJECTFLAGS).cast(),
                    std::mem::size_of::<USEROBJECTFLAGS>() as u32,
                    std::ptr::null_mut(),
                ) != 0)
                .then_some(flags.dwFlags & WSF_VISIBLE != 0);
            let mut state: windows_sys::core::PWSTR = std::ptr::null_mut();
            let mut len = 0u32;
            let disconnected = if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTSConnectState,
                &mut state,
                &mut len,
            ) != 0
                && !state.is_null()
            {
                let s = (len as usize >= std::mem::size_of::<i32>())
                    .then(|| std::ptr::read_unaligned(state as *const i32));
                WTSFreeMemory(state.cast());
                s == Some(WTSDisconnected)
            } else {
                false
            };
            let input = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
            let input_name = (!input.is_null()).then(|| {
                let n = object_name(input);
                CloseDesktop(input);
                n
            });
            let ours = object_name(GetThreadDesktop(GetCurrentThreadId()));
            reach(session, visible, disconnected, input_name.as_deref(), &ours)
        }
    }

    fn out_of_reach_now() -> Result<(), String> {
        match reachable().why() {
            None => Ok(()),
            Some(why) => Err(why.into()),
        }
    }

    /// A window station's or desktop's name.
    unsafe fn object_name(h: HANDLE) -> String {
        let mut buf = [0u16; 128];
        let mut needed = 0u32;
        // SAFETY: `buf` has room for its length in bytes.
        let ok = unsafe {
            GetUserObjectInformationW(
                h,
                UOI_NAME,
                buf.as_mut_ptr().cast(),
                (buf.len() * 2) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return String::new();
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn screens() -> Result<Vec<Screen>, String> {
        unsafe extern "system" fn each(
            m: HMONITOR,
            _dc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> windows_sys::core::BOOL {
            // SAFETY: `data` is the Vec below, alive for the whole call.
            let out = unsafe { &mut *(data as *mut Vec<Screen>) };
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: a monitor Windows just handed us; `info` sized right.
            if unsafe { GetMonitorInfoW(m, &mut info) } != 0 {
                let r = info.rcMonitor;
                if r.right > r.left && r.bottom > r.top {
                    out.push(Screen {
                        x: r.left,
                        y: r.top,
                        w: (r.right - r.left) as u32,
                        h: (r.bottom - r.top) as u32,
                        main: info.dwFlags & 1 != 0,
                    });
                }
            }
            1
        }
        let mut found: Vec<Screen> = Vec::new();
        // SAFETY: the callback only writes to `found`, which outlives the call.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(each),
                &mut found as *mut Vec<Screen> as LPARAM,
            );
        }
        if found.is_empty() {
            return Err("Windows reports no screen (is one connected and on?)".into());
        }
        Ok(ordered(found))
    }

    fn describe_screens(screens: &[Screen]) -> String {
        let mut out = format!(
            "{} screen{}:",
            screens.len(),
            if screens.len() == 1 { "" } else { "s" }
        );
        for (i, s) in screens.iter().enumerate() {
            let (pw, ph) = s.picture();
            out.push_str(&format!(
                "\n{}{}: {}×{} at {},{}; screenshots are {pw}×{ph}",
                i + 1,
                if s.main { " (main)" } else { "" },
                s.w,
                s.h,
                s.x,
                s.y
            ));
        }
        out
    }

    /// A screenshot of screen `n`, with the pointer drawn in.
    fn shot(screens: &[Screen], n: u32) -> Result<(String, NodeImage), Why> {
        let (n, s) = pick(screens, n)?;
        let (pw, ph) = s.picture();
        let bgra = capture(&s, pw, ph)?;
        let png = png_of(&bgra, pw, ph)?;
        let text = format!(
            "Screen {n} of {}{}, {}×{}{}: x/y for clicks are this picture's pixels.",
            screens.len(),
            if s.main { " (main)" } else { "" },
            s.w,
            s.h,
            if (pw, ph) == (s.w, s.h) {
                String::new()
            } else {
                format!(", shown at {pw}×{ph}")
            }
        );
        Ok((
            text,
            NodeImage {
                mime: "image/png".into(),
                data: base64::engine::general_purpose::STANDARD.encode(png),
                width: pw,
                height: ph,
            },
        ))
    }

    fn capture(s: &Screen, pw: u32, ph: u32) -> Result<Vec<u8>, String> {
        // SAFETY: every handle made here is released below; the DIB's bits
        // are read only while it is alive, after GdiFlush.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err("Windows didn't hand over the screen".into());
            }
            let mem = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pw as i32,
                // Negative: rows from the top down.
                biHeight: -(ph as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(
                mem,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            let result = if mem.is_null() || dib.is_null() || bits.is_null() {
                Err("Windows couldn't make room for the screenshot".to_string())
            } else {
                let old = SelectObject(mem, dib);
                SetStretchBltMode(mem, HALFTONE);
                SetBrushOrgEx(mem, 0, 0, std::ptr::null_mut());
                let ok = StretchBlt(
                    mem,
                    0,
                    0,
                    pw as i32,
                    ph as i32,
                    screen,
                    s.x,
                    s.y,
                    s.w as i32,
                    s.h as i32,
                    SRCCOPY | CAPTUREBLT,
                ) != 0;
                if ok {
                    draw_pointer(mem, s);
                }
                GdiFlush();
                let r = if ok {
                    let len = (pw * ph * 4) as usize;
                    Ok(std::slice::from_raw_parts(bits as *const u8, len).to_vec())
                } else {
                    Err(
                        "Windows didn't hand over the picture (the screen may be off or locked)"
                            .into(),
                    )
                };
                SelectObject(mem, old);
                r
            };
            if !dib.is_null() {
                DeleteObject(dib);
            }
            if !mem.is_null() {
                DeleteDC(mem);
            }
            ReleaseDC(std::ptr::null_mut(), screen);
            result
        }
    }

    /// The pointer, where it is, at the picture's scale.
    unsafe fn draw_pointer(mem: HDC, s: &Screen) {
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: plain calls; the icon's bitmaps are deleted after use.
        unsafe {
            if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 || ci.hCursor.is_null()
            {
                return;
            }
            let mut ii: ICONINFO = std::mem::zeroed();
            let (hx, hy) = if GetIconInfo(ci.hCursor, &mut ii) != 0 {
                if !ii.hbmMask.is_null() {
                    DeleteObject(ii.hbmMask);
                }
                if !ii.hbmColor.is_null() {
                    DeleteObject(ii.hbmColor);
                }
                (ii.xHotspot as i32, ii.yHotspot as i32)
            } else {
                (0, 0)
            };
            let k = s.scale();
            let p = ci.ptScreenPos;
            let (cx, cy) = (GetSystemMetrics(SM_CXCURSOR), GetSystemMetrics(SM_CYCURSOR));
            DrawIconEx(
                mem,
                (f64::from(p.x - s.x - hx) * k).round() as i32,
                (f64::from(p.y - s.y - hy) * k).round() as i32,
                ci.hCursor,
                ((f64::from(cx) * k).round() as i32).max(8),
                ((f64::from(cy) * k).round() as i32).max(8),
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            );
        }
    }

    // ---- the mouse and the keyboard --------------------------------------------

    /// Before each step: not asked to stop, the screen still reachable,
    /// and (`target`: the window at that point, or the one in front for
    /// keys) a window Windows lets us send input to.
    fn step(stop: &Stop, target: Option<Option<POINT>>) -> Result<(), Why> {
        go(stop)?;
        // A long action looks again that the guard of Tessera's own windows
        // still sees its input (Windows can take it away meanwhile).
        {
            static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_none_or(|t| t.elapsed() > Duration::from_millis(500)) {
                *last = Some(std::time::Instant::now());
                drop(last);
                crate::node::stopbar::guard_alive().map_err(Why::Refused)?;
            }
        }
        if let Some(why) = reachable().why() {
            return Err(Why::Refused(why.into()));
        }
        if let Some(at) = target {
            window_open_to_us(at)?;
        }
        // Those calls can take a while: a stop meanwhile wins.
        go(stop)
    }

    /// Right before sending anything that isn't a release: not asked to
    /// stop, and the Stop bar still there and answering.
    fn go(stop: &Stop) -> Result<(), Why> {
        if stop.stopped() {
            return Err(Why::Stopped);
        }
        crate::node::stopbar::ready().map_err(Why::Refused)
    }

    fn input(op: &DesktopOp, screens: &[Screen], stop: &Stop) -> Result<NodeResult, Why> {
        let at = |x: i32, y: i32| -> Result<(usize, i32, i32), Why> {
            let (n, s) = pick(screens, op.screen())?;
            let (dx, dy) = s.to_desktop(n, x, y)?;
            Ok((n, dx, dy))
        };
        let on = |n: usize| {
            if screens.len() > 1 {
                format!(" on screen {n}")
            } else {
                String::new()
            }
        };
        let text = match op {
            DesktopOp::Move { x, y, .. } => {
                let (n, dx, dy) = at(*x, *y)?;
                step(stop, None)?;
                move_to(screens, dx, dy, stop)?;
                format!("Moved the pointer to {x},{y}{}.", on(n))
            }
            DesktopOp::Click {
                x,
                y,
                button,
                clicks,
                ..
            } => {
                let (n, dx, dy) = at(*x, *y)?;
                let point = POINT { x: dx, y: dy };
                step(stop, Some(Some(point)))?;
                move_to(screens, dx, dy, stop)?;
                let (down, up) = match button {
                    MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                    MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                    MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                };
                let times = clicks.unwrap_or(1).clamp(1, 3);
                for i in 0..times {
                    if i > 0 && !stop.sleep(Duration::from_millis(60)) {
                        return Err(Why::Stopped);
                    }
                    // Again right before each click: moving may have taken a while.
                    step(stop, Some(Some(point)))?;
                    let pair = [mouse(0, 0, 0, down), mouse(0, 0, 0, up)];
                    let n = send_raw(&pair);
                    if n != 2 {
                        // Only the press went through: let go, or the button stays down.
                        if n == 1 && send_raw(&pair[1..]) != 1 {
                            note_held(&[Held::Button(up)]);
                        }
                        return Err(refused_input(n, 2));
                    }
                }
                let how = match (button, times) {
                    (MouseButton::Left, 2) => "Double-clicked",
                    (MouseButton::Left, 3) => "Triple-clicked",
                    (MouseButton::Left, _) => "Clicked",
                    (MouseButton::Right, _) => "Right-clicked",
                    (MouseButton::Middle, _) => "Middle-clicked",
                };
                format!("{how} at {x},{y}{}.", on(n))
            }
            DesktopOp::Drag {
                x, y, to_x, to_y, ..
            } => {
                let (n, ax, ay) = at(*x, *y)?;
                let (_, bx, by) = at(*to_x, *to_y)?;
                // Where it starts and where it lands.
                step(stop, Some(Some(POINT { x: bx, y: by })))?;
                step(stop, Some(Some(POINT { x: ax, y: ay })))?;
                move_to(screens, ax, ay, stop)?;
                step(stop, Some(Some(POINT { x: ax, y: ay })))?;
                // Noted as held before it is: the user's Stop lets go of it
                // at once, whatever this thread is waiting for (a release
                // of a button not yet down does nothing).
                if !hold(Held::Button(MOUSEEVENTF_LEFTUP), true) {
                    hold(Held::Button(MOUSEEVENTF_LEFTUP), false);
                    return Err(Why::Failed(
                        "the button a drag holds couldn't be written down (so it could be let go if Tessera were ended meanwhile), so the drag wasn't started".into(),
                    ));
                }
                if let Err(why) = send(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTDOWN)]) {
                    hold(Held::Button(MOUSEEVENTF_LEFTUP), false);
                    return Err(why);
                }
                let mut moved = if stop.sleep(Duration::from_millis(80)) {
                    Ok(())
                } else {
                    Err(Why::Stopped)
                };
                let steps = 12;
                for i in 1..=steps {
                    if moved.is_err() {
                        break;
                    }
                    let px = ax + (bx - ax) * i / steps;
                    let py = ay + (by - ay) * i / steps;
                    moved = step(stop, Some(Some(POINT { x: bx, y: by })))
                        .and_then(|()| move_to(screens, px, py, stop));
                    if moved.is_ok() && !stop.sleep(Duration::from_millis(20)) {
                        moved = Err(Why::Stopped);
                    }
                }
                // A moment for the drop to land, unless stopped: then at once.
                if moved.is_ok() {
                    std::thread::sleep(Duration::from_millis(80));
                }
                // Let go whatever happened, so the button isn't left down.
                let released = send_raw(&[mouse(0, 0, 0, MOUSEEVENTF_LEFTUP)]) == 1;
                hold(Held::Button(MOUSEEVENTF_LEFTUP), false);
                if !released {
                    note_held(&[Held::Button(MOUSEEVENTF_LEFTUP)]);
                    return Err(match moved {
                        // Said along with why it ended early.
                        Err(why) => with_held(why),
                        Ok(()) => refused_input(0, 1),
                    });
                }
                moved?;
                format!("Dragged from {x},{y} to {to_x},{to_y}{}.", on(n))
            }
            DesktopOp::Scroll { x, y, dx, dy, .. } => {
                let (n, px, py) = at(*x, *y)?;
                step(stop, Some(Some(POINT { x: px, y: py })))?;
                move_to(screens, px, py, stop)?;
                if *dy != 0 {
                    step(stop, Some(Some(POINT { x: px, y: py })))?;
                    send(&[mouse(0, 0, (-dy * WHEEL_DELTA) as u32, MOUSEEVENTF_WHEEL)])?;
                }
                if *dx != 0 {
                    step(stop, Some(Some(POINT { x: px, y: py })))?;
                    send(&[mouse(0, 0, (dx * WHEEL_DELTA) as u32, MOUSEEVENTF_HWHEEL)])?;
                }
                let mut how = Vec::new();
                if *dy != 0 {
                    how.push(format!(
                        "{} {}",
                        if *dy > 0 { "down" } else { "up" },
                        dy.abs()
                    ));
                }
                if *dx != 0 {
                    how.push(format!(
                        "{} {}",
                        if *dx > 0 { "right" } else { "left" },
                        dx.abs()
                    ));
                }
                format!("Scrolled {} at {x},{y}{}.", how.join(" and "), on(n))
            }
            DesktopOp::Type { text } => {
                let total = text.chars().count();
                if total > MAX_TYPE {
                    return Err(Why::Failed(format!(
                        "that is more than {MAX_TYPE} characters; type it in parts"
                    )));
                }
                let typed = type_text(text, stop);
                match typed {
                    Ok(()) => format!(
                        "Typed {total} character{}.",
                        if total == 1 { "" } else { "s" }
                    ),
                    Err((done, why)) if done > 0 => {
                        return Err(match why {
                            Why::Refused(w) => {
                                Why::Refused(format!("{w} (after {done} of {total} characters)"))
                            }
                            Why::Failed(w) => {
                                Why::Failed(format!("{w} (after {done} of {total} characters)"))
                            }
                            Why::Stopped => {
                                Why::Failed(format!("stopped after {done} of {total} characters"))
                            }
                        });
                    }
                    Err((_, why)) => return Err(why),
                }
            }
            DesktopOp::Key { keys } => {
                let combos = parse_keys(keys)?;
                for (i, combo) in combos.iter().enumerate() {
                    if i > 0 && !stop.sleep(Duration::from_millis(60)) {
                        return Err(Why::Stopped);
                    }
                    // The window in front may have changed with the last one (alt+tab).
                    step(stop, Some(None))?;
                    press(combo, stop)?;
                }
                format!(
                    "Pressed {}.",
                    keys.split_whitespace().collect::<Vec<_>>().join(", then ")
                )
            }
            DesktopOp::Screens | DesktopOp::Screenshot { .. } => String::new(),
        };
        Ok(done(text, None))
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INPUT_MARK,
                },
            },
        }
    }

    fn key(vk: u16, scan: u16, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INPUT_MARK,
                },
            },
        }
    }

    /// A key press or release as Windows wants it (scan code, extended).
    fn vk_event(vk: u16, up: bool) -> INPUT {
        // SAFETY: plain call.
        let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
        let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
        if extended(vk) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        key(vk, scan, flags)
    }

    fn release_of(h: Held) -> INPUT {
        match h {
            Held::Key(vk) => vk_event(vk, true),
            Held::Char(u) => key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            Held::Button(up) => mouse(0, 0, 0, up),
        }
    }

    /// Input Windows refused to let go of, kept until it does.
    static HELD: std::sync::Mutex<Vec<Held>> = std::sync::Mutex::new(Vec::new());

    fn note_held(held: &[Held]) {
        {
            let mut h = HELD.lock().unwrap_or_else(|e| e.into_inner());
            for x in held {
                if !h.contains(x) {
                    h.push(*x);
                }
            }
        }
        if !held.is_empty() {
            write_held();
        }
    }

    /// Let go of what an earlier action couldn't; an error (nothing else is
    /// done) while Windows still won't.
    fn release_held() -> Result<(), String> {
        let mut h = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if h.is_empty() {
            return Ok(());
        }
        let releases: Vec<INPUT> = h.iter().map(|x| release_of(*x)).collect();
        let took = send_raw(&releases);
        *h = left_held(&h, took);
        let done = h.is_empty();
        drop(h);
        write_held();
        let h = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if done {
            return Ok(());
        }
        Err(format!(
            "Windows may still hold down {} from an earlier desktop action and didn't let Tessera release it; nothing more is done until it does. Ask the user to press and release it (or Ctrl, Alt and Shift), then try again",
            h.iter()
                .map(|x| match x {
                    Held::Key(vk) => format!("key 0x{vk:02X}"),
                    // Never which one: what was typed stays out of logs.
                    Held::Char(_) => "a typed character's key".to_string(),
                    Held::Button(_) => "a mouse button".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// How many of `inputs` Windows took.
    fn send_raw(inputs: &[INPUT]) -> usize {
        // SAFETY: a valid array of INPUTs and its element size.
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            ) as usize
        }
    }

    fn send(inputs: &[INPUT]) -> Result<(), Why> {
        let n = send_raw(inputs);
        if n == inputs.len() {
            Ok(())
        } else {
            Err(refused_input(n, inputs.len()))
        }
    }

    /// Why Windows took only `n` of `of` input events (and whether it may
    /// still hold something down).
    fn refused_input(n: usize, of: usize) -> Why {
        let err = std::io::Error::last_os_error();
        let held = held_note();
        // Blocked: most often the secure desktop came up in between.
        match reachable().why() {
            Some(why) => Why::Refused(format!("{why}{held}")),
            None => Why::Failed(format!(
                "Windows took only {n} of {of} input events ({err}){held}"
            )),
        }
    }

    /// Said with any error while Windows may still hold something down.
    fn held_note() -> &'static str {
        if HELD.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            ""
        } else {
            ". Windows may still hold a key or mouse button down: the next desktop action lets go of it first"
        }
    }

    /// `why` an action ended early, and that something may still be held.
    fn with_held(why: Why) -> Why {
        let held = held_note();
        match why {
            Why::Refused(w) => Why::Refused(format!("{w}{held}")),
            Why::Failed(w) => Why::Failed(format!("{w}{held}")),
            Why::Stopped if held.is_empty() => Why::Stopped,
            Why::Stopped => Why::Failed(format!("{STOPPED}{held}")),
        }
    }

    /// Move the pointer to `x`,`y` of the desktop, as a real mouse would.
    fn move_to(screens: &[Screen], x: i32, y: i32, stop: &Stop) -> Result<(), Why> {
        let left = screens.iter().map(|s| s.x).min().unwrap_or(0);
        let top = screens.iter().map(|s| s.y).min().unwrap_or(0);
        let right = screens.iter().map(|s| s.x + s.w as i32).max().unwrap_or(1);
        let bottom = screens.iter().map(|s| s.y + s.h as i32).max().unwrap_or(1);
        go(stop)?;
        send(&[mouse(
            absolute(x, left, right),
            absolute(y, top, bottom),
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )])?;
        // Not even the cursor fix-up once asked to stop.
        go(stop)?;
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: plain call.
        let off =
            unsafe { GetCursorPos(&mut p) } != 0 && ((p.x - x).abs() > 1 || (p.y - y).abs() > 1);
        if off {
            // That call can take a while too.
            go(stop)?;
            // SAFETY: plain call.
            unsafe { SetCursorPos(x, y) };
        }
        Ok(())
    }

    /// Type `text` in batches, looking before each one (stopped, locked,
    /// another window in front); on an error, how many characters surely
    /// went (`typed_chars`).
    fn type_text(text: &str, stop: &Stop) -> Result<(), (usize, Why)> {
        let plan = type_plan(text);
        let event = |(k, up, _): &(TypeKey, bool, usize)| {
            let up = if *up { KEYEVENTF_KEYUP } else { 0 };
            match k {
                TypeKey::Vk(vk) => key(*vk, 0, up),
                TypeKey::Unit(u) => key(0, *u, KEYEVENTF_UNICODE | up),
            }
        };
        let mut took = 0usize;
        // Batches of whole characters (a stop never falls inside one).
        // Few keys at a time: where they go is looked at again before each.
        for batch in type_batches(&plan, 8) {
            let chunk = &plan[batch];
            step(stop, Some(None)).map_err(|w| (typed_chars(&plan, took), w))?;
            let inputs: Vec<INPUT> = chunk.iter().map(event).collect();
            go(stop).map_err(|w| (typed_chars(&plan, took), w))?;
            // Noted as held while Windows takes them (see `press`).
            let keys: Vec<Held> = chunk.iter().map(|e| typed_held(e.0)).collect();
            for h in &keys {
                hold(*h, true);
            }
            let n = send_raw(&inputs);
            for h in &keys {
                hold(*h, false);
            }
            took += n;
            if n != inputs.len() {
                // Cut off after a press: its release all the same (it counts
                // when it goes through), kept if Windows refuses that too.
                if n % 2 == 1 {
                    if send_raw(&inputs[n..n + 1]) == 1 {
                        took += 1;
                    } else {
                        note_held(&[typed_held(chunk[n - 1].0)]);
                    }
                }
                return Err((typed_chars(&plan, took), refused_input(n, inputs.len())));
            }
            if !stop.sleep(Duration::from_millis(2)) {
                return Err((typed_chars(&plan, took), Why::Stopped));
            }
        }
        Ok(())
    }

    fn typed_held(k: TypeKey) -> Held {
        match k {
            TypeKey::Vk(vk) => Held::Key(vk),
            TypeKey::Unit(u) => Held::Char(u),
        }
    }

    /// Press one combination: its keys down in order, then up in reverse.
    fn press(combo: &[Key], stop: &Stop) -> Result<(), Why> {
        let mut vks: Vec<u16> = Vec::new();
        for k in combo {
            match k {
                Key::Vk(vk) => vks.push(*vk),
                Key::Char(c) => {
                    let mut units = [0u16; 2];
                    let u = c.encode_utf16(&mut units);
                    // SAFETY: plain call.
                    let scan = if u.len() == 1 {
                        unsafe { VkKeyScanW(u[0]) }
                    } else {
                        -1
                    };
                    if scan == -1 {
                        return Err(Why::Failed(format!(
                            "`{c}` isn't on this computer's keyboard layout; use `type` for characters"
                        )));
                    }
                    let (vk, shift) = ((scan & 0xFF) as u16, (scan >> 8) & 1 != 0);
                    if shift {
                        vks.push(0x10);
                    }
                    vks.push(vk);
                }
            }
        }
        let events = combo_events(&vks);
        let inputs: Vec<INPUT> = events.iter().map(|(vk, up)| vk_event(*vk, *up)).collect();
        go(stop)?;
        // Noted as held before they are: the user's Stop lets go of them at
        // once, even while this thread waits in Windows' call.
        for vk in &vks {
            hold(Held::Key(*vk), true);
        }
        let n = send_raw(&inputs);
        for vk in &vks {
            hold(Held::Key(*vk), false);
        }
        if n == inputs.len() {
            return Ok(());
        }
        // Cut off halfway: let go of what is still held (a modifier left
        // down would change every key the user presses next); what Windows
        // won't let go of is kept, and released before anything else.
        let held: Vec<Held> = still_down(&events, n).into_iter().map(Held::Key).collect();
        let releases: Vec<INPUT> = held.iter().map(|h| release_of(*h)).collect();
        let took = send_raw(&releases);
        note_held(&left_held(&held, took));
        Err(refused_input(n, inputs.len()))
    }

    /// The window at `at` (the one in front, for keys) belongs to a program
    /// Windows lets us send input to: not one of higher integrity (run as
    /// administrator), whose input Windows would drop without a word.
    fn window_open_to_us(at: Option<POINT>) -> Result<(), Why> {
        // SAFETY: plain calls; the process and token handles are closed.
        unsafe {
            let w: HWND = match at {
                Some(p) => WindowFromPoint(p),
                None => GetForegroundWindow(),
            };
            if w.is_null() {
                return Ok(());
            }
            let root = GetAncestor(w, GA_ROOT);
            let mut pid = 0u32;
            GetWindowThreadProcessId(if root.is_null() { w } else { root }, &mut pid);
            if pid == 0 {
                return Ok(());
            }
            // The Stop bar is ours.
            if pid == GetCurrentProcessId() {
                return Err(Why::Refused(TESSERA_WINDOW.into()));
            }
            let ours = integrity(GetCurrentProcess());
            let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            let (theirs, program) = if p.is_null() {
                (None, None)
            } else {
                let i = integrity(p);
                let program = image_of(p);
                CloseHandle(p);
                (i, program)
            };
            let here = std::env::current_exe().ok();
            if program
                .is_some_and(|p| is_tessera_program(&p, here.as_ref().and_then(|h| h.parent())))
            {
                return Err(Why::Refused(TESSERA_WINDOW.into()));
            }
            if may_send_to(ours, theirs) {
                Ok(())
            } else {
                Err(Why::Refused(ADMIN.into()))
            }
        }
    }

    /// Whether window `w` is one of Tessera's own (this program's, or one
    /// of its programs'). For the Stop bar's guard, which asks for every
    /// marked click and key: what a process is stays known for a while.
    pub fn tessera_window(w: HWND) -> bool {
        use std::collections::HashMap;
        use std::time::Instant;
        static KNOWN: std::sync::Mutex<Option<(Instant, HashMap<u32, bool>)>> =
            std::sync::Mutex::new(None);
        if w.is_null() {
            return false;
        }
        // SAFETY: plain calls; the process handle is closed.
        unsafe {
            let root = GetAncestor(w, GA_ROOT);
            let mut pid = 0u32;
            GetWindowThreadProcessId(if root.is_null() { w } else { root }, &mut pid);
            if pid == 0 {
                return false;
            }
            if pid == GetCurrentProcessId() {
                return true;
            }
            let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
            let (since, map) = known.get_or_insert_with(|| (Instant::now(), HashMap::new()));
            // Process ids come back: forgotten every half minute.
            if since.elapsed() > Duration::from_secs(30) {
                *since = Instant::now();
                map.clear();
            }
            if let Some(ours) = map.get(&pid) {
                return *ours;
            }
            let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            let program = if p.is_null() {
                None
            } else {
                let program = image_of(p);
                CloseHandle(p);
                program
            };
            // A program Windows won't name may be one of Tessera's: refused
            // (input to it would mostly be dropped anyway), and asked again
            // next time rather than remembered.
            let Some(program) = program else {
                return true;
            };
            let here = std::env::current_exe().ok();
            let ours = is_tessera_program(&program, here.as_ref().and_then(|h| h.parent()));
            map.insert(pid, ours);
            ours
        }
    }

    /// A process's program file.
    unsafe fn image_of(process: HANDLE) -> Option<std::path::PathBuf> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: a valid process handle and a buffer of `len` units.
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
        (ok != 0).then(|| {
            use std::os::windows::ffi::OsStringExt;
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]))
        })
    }

    /// A process's integrity level (e.g. 0x2000 medium, 0x3000 high).
    unsafe fn integrity(process: HANDLE) -> Option<u32> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: valid process handle; the token is closed below.
        unsafe {
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            // Aligned for the label it holds.
            let mut buf = [0u64; 16];
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenIntegrityLevel,
                buf.as_mut_ptr().cast(),
                std::mem::size_of_val(&buf) as u32,
                &mut len,
            ) != 0;
            let level = if ok {
                let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
                let sid = label.Label.Sid;
                let count = *GetSidSubAuthorityCount(sid);
                (count > 0).then(|| *GetSidSubAuthority(sid, u32::from(count) - 1))
            } else {
                None
            };
            CloseHandle(token);
            level
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: i32, y: i32, w: u32, h: u32, main: bool) -> Screen {
        Screen { x, y, w, h, main }
    }

    #[test]
    fn pictures_fit_and_their_pixels_map_back_onto_the_screen() {
        let big = screen(0, 0, 2560, 1440, true);
        assert_eq!(big.picture(), (1280, 720));
        // A picture pixel stands for 2×2 screen pixels here: the middle one of them.
        assert_eq!(big.to_desktop(1, 0, 0), Ok((1, 1)));
        assert_eq!(big.to_desktop(1, 640, 360), Ok((1281, 721)));
        assert_eq!(big.to_desktop(1, 1279, 719), Ok((2559, 1439)));
        assert!(big.to_desktop(1, 1280, 10).is_err());
        assert!(big.to_desktop(1, -1, 10).is_err());
        // Small screens aren't blown up; a second screen keeps its place.
        let small = screen(-1024, 200, 1024, 768, false);
        assert_eq!(small.picture(), (1024, 768));
        assert_eq!(small.to_desktop(2, 10, 20), Ok((-1014, 220)));
        // Tall ones fit the height.
        let tall = screen(0, 0, 1080, 1920, false);
        assert_eq!(tall.picture(), (450, 800));
        let e = tall.to_desktop(3, 500, 10).unwrap_err();
        assert!(e.contains("screen 3") && e.contains("450×800"), "{e}");
    }

    #[test]
    fn the_main_screen_comes_first() {
        let s = ordered(vec![
            screen(1920, 0, 1920, 1080, false),
            screen(-1280, 0, 1280, 1024, false),
            screen(0, 0, 1920, 1080, true),
        ]);
        assert_eq!(
            s.iter().map(|s| s.x).collect::<Vec<_>>(),
            vec![0, -1280, 1920]
        );
        assert!(pick(&s, 3).is_ok());
        assert!(pick(&s, 4).unwrap_err().contains("3 screens"));
    }

    #[test]
    fn keys_are_read_by_name() {
        assert_eq!(
            parse_keys("ctrl+shift+esc").unwrap(),
            vec![vec![Key::Vk(0x11), Key::Vk(0x10), Key::Vk(0x1B)]]
        );
        assert_eq!(
            parse_keys("ctrl+a  Ctrl+C").unwrap(),
            vec![
                vec![Key::Vk(0x11), Key::Vk(0x41)],
                vec![Key::Vk(0x11), Key::Vk(0x43)]
            ]
        );
        assert_eq!(parse_keys("f5").unwrap(), vec![vec![Key::Vk(0x74)]]);
        assert_eq!(
            parse_keys("win+r").unwrap(),
            vec![vec![Key::Vk(0x5B), Key::Vk(0x52)]]
        );
        assert_eq!(
            parse_keys("ctrl++").unwrap(),
            vec![vec![Key::Vk(0x11), Key::Char('+')]]
        );
        assert_eq!(parse_keys("/").unwrap(), vec![vec![Key::Char('/')]]);
        assert!(
            parse_keys("ctrl+alt+delete")
                .unwrap_err()
                .contains("Ctrl+Alt+Delete")
        );
        assert!(parse_keys("ctrl+banana").unwrap_err().contains("banana"));
        assert!(parse_keys("   ").is_err());
        assert!(extended(0x25) && extended(0x2E) && !extended(0x41));
    }

    #[test]
    fn the_secure_screen_a_disconnected_session_and_services_are_out_of_reach() {
        // Signed in, on screen.
        assert_eq!(
            reach(Some(1), Some(true), false, Some("Default"), "Default"),
            Reach::Ok
        );
        // Locked, a UAC prompt or Ctrl+Alt+Delete: Windows won't open the
        // input desktop for us, or names its own.
        assert_eq!(
            reach(Some(1), Some(true), false, None, "Default"),
            Reach::Secure
        );
        assert_eq!(
            reach(Some(1), Some(true), false, Some("Winlogon"), "Default"),
            Reach::Secure
        );
        // A closed remote desktop.
        assert_eq!(
            reach(Some(2), Some(true), true, Some("Default"), "Default"),
            Reach::Disconnected
        );
        // A service, an invisible station, another desktop.
        assert_eq!(
            reach(Some(0), Some(true), false, Some("Default"), "Default"),
            Reach::Service
        );
        assert_eq!(
            reach(Some(1), Some(false), false, Some("Default"), "Default"),
            Reach::Invisible
        );
        assert_eq!(
            reach(Some(1), Some(true), false, Some("Default"), "Hidden"),
            Reach::Elsewhere
        );
        for r in [
            Reach::Secure,
            Reach::Disconnected,
            Reach::Service,
            Reach::Invisible,
            Reach::Elsewhere,
        ] {
            assert!(r.why().is_some_and(|w| w.len() > 40), "{r:?}");
        }
        assert!(Reach::Secure.why().unwrap().contains("by design"));
        assert_eq!(Reach::Ok.why(), None);
    }

    #[test]
    fn input_goes_only_to_programs_of_the_same_level_or_lower() {
        const MEDIUM: u32 = 0x2000;
        const HIGH: u32 = 0x3000;
        assert!(may_send_to(Some(MEDIUM), Some(MEDIUM)));
        assert!(may_send_to(Some(HIGH), Some(MEDIUM)));
        // Run as administrator, or Windows won't say: kept apart.
        assert!(!may_send_to(Some(MEDIUM), Some(HIGH)));
        assert!(!may_send_to(Some(MEDIUM), None));
        // Ours unknown: nothing.
        assert!(!may_send_to(None, Some(MEDIUM)));
        assert!(!may_send_to(None, None));
    }

    #[test]
    fn desktop_control_only_from_the_users_normal_level() {
        assert!(may_act_as(Some(0x2000)).is_ok());
        assert!(may_act_as(Some(0x1000)).is_ok());
        assert!(
            may_act_as(Some(HIGH_INTEGRITY))
                .unwrap_err()
                .contains("administrator")
        );
        assert!(may_act_as(Some(0x4000)).is_err());
        assert!(may_act_as(None).is_err());
    }

    #[test]
    fn tessera_s_own_windows_are_never_used_by_agents() {
        use std::path::Path;
        let here = Path::new(r"C:\Users\me\AppData\Local\Tessera");
        // Next to this program (the app installs them together), or by name.
        for p in [
            r"C:\Users\me\AppData\Local\Tessera\tessera-app.exe",
            r"c:\users\ME\appdata\local\tessera\anything.exe",
            r"D:\elsewhere\Tessera.exe",
            r"D:\elsewhere\tessera-service.exe",
        ] {
            assert!(is_tessera_program(Path::new(p), Some(here)), "{p}");
        }
        for p in [
            r"C:\Windows\notepad.exe",
            r"C:\Program Files\Mozilla Firefox\firefox.exe",
            r"C:\Users\me\AppData\Local\Tessera\sub\tool.exe",
        ] {
            assert!(!is_tessera_program(Path::new(p), Some(here)), "{p}");
        }
        assert!(!is_tessera_program(Path::new(r"C:\x\notepad.exe"), None));
    }

    #[test]
    fn the_user_s_stop_ends_every_action_and_none_starts_until_resumed() {
        // Its own Stop switch: the computer's one is shared by every test.
        static HERE: Halt = Halt::new();
        let before = Stop::under(&HERE);
        assert!(!before.stopped());
        HERE.halt();
        assert!(HERE.halted());
        // Running ones stop; one begun while stopped is stopped too.
        assert!(before.stopped());
        let during = Stop::under(&HERE);
        assert!(during.stopped());
        let r = act(&DesktopOp::Screens, false, &during);
        assert!(!r.ok, "{r:?}");
        HERE.resume();
        assert!(!HERE.halted());
        // Begun before or during the Stop: stopped for good after the resume.
        assert!(before.stopped() && during.stopped());
        assert!(!before.sleep(std::time::Duration::from_millis(5)));
        let after = Stop::under(&HERE);
        assert!(!after.stopped());
        // Nothing held here: letting go returns at once.
        let t = std::time::Instant::now();
        let_go(begin_let_go());
        assert!(!letting_go());
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn pointer_positions_count_from_the_desktops_own_edge() {
        // One 1920-wide screen: its edges and middle.
        assert_eq!(absolute(0, 0, 1920), 17);
        assert_eq!(absolute(960, 0, 1920), 32785);
        assert_eq!(absolute(1919, 0, 1920), 65518);
        // A screen left of the main one: the desktop starts at -1280.
        assert_eq!(absolute(-1280, -1280, 1920), absolute(0, 0, 3200));
        assert_eq!(absolute(0, -1280, 1920), absolute(1280, 0, 3200));
        // Never outside 0..=65535.
        assert_eq!(absolute(-5000, -1280, 1920), 0);
        assert_eq!(absolute(9000, -1280, 1920), 65535);
        // A screen above, scaled 150%: positions are in real pixels, so the
        // same arithmetic holds (2880×1620 at 0,-1620 over a 1920×1080 main).
        let above = screen(0, -1620, 2880, 1620, false);
        let (_, y) = above.to_desktop(2, 0, 0).unwrap();
        assert!((-1620..-1617).contains(&y), "{y}");
        assert!(absolute(y, -1620, 1080) < absolute(0, -1620, 1080));
    }

    #[test]
    fn the_screen_is_one_chats_at_a_time() {
        let s = std::time::Duration::from_secs;
        let t0 = std::time::Instant::now();
        let mut lease = Lease::default();
        assert!(lease.begin("chat-a", t0).is_ok());
        // While its action runs, nobody else, however long it takes.
        assert_eq!(lease.begin("chat-b", t0 + s(300)), Err(LEASE));
        lease.end("chat-a", t0 + s(300));
        // Then a minute from when it ended; the same chat goes on.
        assert_eq!(lease.begin("chat-b", t0 + s(310)), Err(LEASE - s(10)));
        assert!(lease.begin("chat-a", t0 + s(350)).is_ok());
        lease.end("chat-a", t0 + s(351));
        // Another chat's ending changes nothing.
        lease.end("chat-b", t0 + s(352));
        assert!(lease.begin("chat-b", t0 + s(400)).is_err());
        assert!(lease.begin("chat-b", t0 + s(411)).is_ok());
        assert!(lease.begin("chat-a", t0 + s(412)).is_err());
    }

    #[test]
    fn a_stop_ends_waiting_and_what_isnt_let_go_is_kept() {
        let stop = Stop::default();
        assert!(stop.sleep(std::time::Duration::from_millis(5)));
        let other = stop.clone();
        let t = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            other.stop();
        });
        assert!(!stop.sleep(std::time::Duration::from_secs(10)));
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        assert!(stop.stopped());
        // Released all, some, none.
        let held = [Held::Key(0x11), Held::Key(0x10), Held::Button(4)];
        assert!(left_held(&held, 3).is_empty());
        assert_eq!(left_held(&held, 1), vec![Held::Key(0x10), Held::Button(4)]);
        assert_eq!(left_held(&held, 0), held.to_vec());
        // Stopped before it began: nothing happens.
        let r = act(&DesktopOp::Screens, false, &stop);
        assert!(!r.ok && r.output.starts_with("stopped"), "{r:?}");
    }

    #[test]
    fn typing_counts_only_what_surely_went() {
        // "a", CR LF (one Enter, two characters), a character in two units.
        let plan = type_plan("a\r\n😀");
        let keys: Vec<(TypeKey, bool)> = plan.iter().map(|(k, up, _)| (*k, *up)).collect();
        assert_eq!(
            keys,
            vec![
                (TypeKey::Unit('a' as u16), false),
                (TypeKey::Unit('a' as u16), true),
                (TypeKey::Vk(0x0D), false),
                (TypeKey::Vk(0x0D), true),
                (TypeKey::Unit(0xD83D), false),
                (TypeKey::Unit(0xD83D), true),
                (TypeKey::Unit(0xDE00), false),
                (TypeKey::Unit(0xDE00), true),
            ]
        );
        let done: Vec<usize> = (0..=plan.len()).map(|n| typed_chars(&plan, n)).collect();
        // A character counts once its last release went through.
        assert_eq!(done, vec![0, 0, 1, 1, 3, 3, 3, 3, 4]);
        assert!(type_plan("").is_empty());
        assert_eq!(typed_chars(&type_plan("\t\n"), 4), 2);
    }

    #[test]
    fn typing_is_sent_in_whole_characters() {
        // 31 letters (62 events), then a character in two units: a batch of
        // 64 would end between its halves.
        let text = format!("{}😀b", "a".repeat(31));
        let plan = type_plan(&text);
        let batches = type_batches(&plan, 64);
        assert_eq!(batches, vec![0..62, 62..68]);
        // Every batch ends where a character is complete.
        let mut shapes = vec![text.clone(), "\r\n".repeat(40), "😀".repeat(40), "x".into()];
        shapes.push(format!("a{}", "😀\r\n\t".repeat(30)));
        for text in shapes {
            let plan = type_plan(&text);
            for max in [4, 5, 7, 64] {
                let batches = type_batches(&plan, max);
                assert_eq!(batches.first().map(|b| b.start), Some(0));
                assert_eq!(batches.last().map(|b| b.end), Some(plan.len()));
                for (i, b) in batches.iter().enumerate() {
                    assert!(!b.is_empty() && b.len() <= max, "{max}: {b:?}");
                    if i > 0 {
                        assert_eq!(batches[i - 1].end, b.start);
                    }
                    // Whole characters: the count moves on at its end.
                    assert!(typed_chars(&plan, b.end) > typed_chars(&plan, b.end - 1));
                }
                let total = text.chars().count();
                assert_eq!(typed_chars(&plan, plan.len()), total);
            }
        }
        assert!(type_batches(&[], 64).is_empty());
    }

    #[test]
    fn keys_are_let_go_even_when_cut_off() {
        let ev = combo_events(&[0x43, 0x11, 0x10]);
        // Modifiers first, the rest after, all up in reverse.
        assert_eq!(
            ev,
            vec![
                (0x11, false),
                (0x10, false),
                (0x43, false),
                (0x43, true),
                (0x10, true),
                (0x11, true)
            ]
        );
        // Windows took only the first two presses: both are let go, newest first.
        assert_eq!(still_down(&ev, 2), vec![0x10, 0x11]);
        assert_eq!(still_down(&ev, 4), vec![0x10, 0x11]);
        assert_eq!(still_down(&ev, ev.len()), Vec::<u16>::new());
        assert_eq!(still_down(&ev, 0), Vec::<u16>::new());
        // A key named twice is pressed once.
        assert_eq!(combo_events(&[0x10, 0x10, 0x41]).len(), 4);
    }

    #[test]
    fn a_screenshot_becomes_a_png() {
        // Two pixels: blue, then red (Windows' order is blue, green, red).
        let png = png_of(&[255, 0, 0, 0, 0, 0, 255, 0], 2, 1).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..6], &[0, 0, 255, 255, 0, 0]);
    }

    #[test]
    fn without_desktop_support_nothing_is_done() {
        if AVAILABLE {
            return;
        }
        let r = act(&DesktopOp::Screens, false, &Stop::default());
        assert!(r.refused && !r.ok, "{r:?}");
    }
}
