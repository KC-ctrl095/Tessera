//! The Stop bar (Windows): a slim bar at the top of the main screen says
//! which chat is doing what on this computer, with a Stop button; a key
//! (Ctrl+Alt+Esc, or the next free one) does the same from any program. It
//! shows all the time while desktop control is on, otherwise while agents
//! work here (and a minute after); the key works whenever the machine runs. Stop ends every action at once, lets go of
//! keys and buttons, and keeps this machine stopped until the user resumes
//! it here: Resume on the bar with their own mouse, This computer in the
//! app, or `tessera node resume`. Agents can't: their input to Tessera's
//! windows is refused, a stopped machine does nothing, and the bar ignores
//! clicks that programs send.
//!
//! Without the bar and its key, desktop actions are refused ([`ready`]):
//! the user must always be able to see and stop them.

// Elsewhere only the plain parts are used (and tested).
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// What the user did on the bar (or with its key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarEvent {
    Stop,
    Resume,
}

/// The keys tried for Stop, in order: the first one no other program holds.
pub const KEYS: [&str; 3] = ["Ctrl+Alt+Esc", "Ctrl+Alt+Pause", "Ctrl+Alt+Shift+S"];

/// How long the bar still names the last chat after it finished.
const RECENT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Doing {
    chat: String,
    what: String,
}

#[derive(Default)]
struct Shared {
    /// The machine runs here (the key works, the bar can show); desktop
    /// control is on (the bar always shows); stopped by the user.
    on: bool,
    desktop: bool,
    stopped: bool,
    /// Actions under way, by call; the last one that ended, and when.
    doing: BTreeMap<String, (u64, Doing)>,
    seq: u64,
    last: Option<(Doing, Instant)>,
    /// The bar's thread was started; its window (0: none), its key, and
    /// why it couldn't start.
    started: bool,
    window: isize,
    key: Option<&'static str>,
    failed: Option<String>,
    /// A word for a moment ("use your own mouse to resume").
    note: Option<(String, Instant)>,
    /// The bar's thread last answered (its half-second tick); its window
    /// has its place on the main screen, and is on screen.
    beat: Option<Instant>,
    placed: bool,
    visible: bool,
    /// With desktop control: the guard that keeps agents' clicks and keys
    /// off Tessera's own windows where they arrive is in place.
    guarded: bool,
    /// The newest action ([`begin`]'s number) the bar was on screen for.
    drawn: u64,
    events: Option<tokio::sync::mpsc::UnboundedSender<BarEvent>>,
}

impl Shared {
    /// The bar is on screen: always with desktop control or once stopped,
    /// otherwise while agents work here and a minute after.
    fn shown(&self, now: Instant) -> bool {
        self.on
            && (self.desktop
                || self.stopped
                || !self.doing.is_empty()
                || self
                    .last
                    .as_ref()
                    .is_some_and(|(_, at)| now.duration_since(*at) < RECENT))
    }
}

static BAR: LazyLock<Mutex<Shared>> = LazyLock::new(Default::default);

fn bar() -> std::sync::MutexGuard<'static, Shared> {
    BAR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the user's Stop and Resume go (the machine's loop).
pub fn listen(events: tokio::sync::mpsc::UnboundedSender<BarEvent>) {
    bar().events = Some(events);
}

/// The machine runs here (`on`), with desktop control or not, stopped or
/// not: shows, updates or hides the bar (starting it the first time, or
/// again after it couldn't).
pub fn sync(on: bool, desktop: bool, stopped: bool) {
    let start = {
        let mut b = bar();
        b.on = on;
        b.desktop = desktop;
        b.stopped = stopped;
        let start = on && cfg!(windows) && (!b.started || (b.window == 0 && b.failed.is_some()));
        if start {
            b.started = true;
            b.failed = None;
        }
        start
    };
    if start {
        imp::start();
    } else {
        imp::refresh(true);
    }
}

/// An action began (`what`: in a few words, never typed text). Its number,
/// for [`shown`].
pub fn begin(call: &str, chat: &str, what: &str) -> u64 {
    let seq = {
        let mut b = bar();
        b.seq += 1;
        let seq = b.seq;
        b.doing.insert(
            call.to_string(),
            (
                seq,
                Doing {
                    chat: chat.to_string(),
                    what: what.to_string(),
                },
            ),
        );
        seq
    };
    imp::refresh(false);
    seq
}

/// The bar came on screen for action `seq` (or a later one) within `limit`
/// (Windows; elsewhere there is no bar): nothing runs here unseen.
pub async fn shown(seq: u64, limit: Duration) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let until = Instant::now() + limit;
    loop {
        {
            let b = bar();
            if b.drawn >= seq && b.visible && b.placed {
                return true;
            }
        }
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub fn end(call: &str) {
    {
        let mut b = bar();
        if let Some((_, d)) = b.doing.remove(call) {
            b.last = Some((d, Instant::now()));
        }
    }
    imp::refresh(false);
}

/// Desktop actions may go ahead: the bar is up with its key. `Err` says
/// why not, for the chat.
pub fn ready() -> Result<(), String> {
    match not_ready() {
        Some(why) => Err(format!(
            "this computer's Stop bar isn't working ({why}), so agents can't use its screen: the user must always be able to see and stop them. {RESTART}"
        )),
        None => Ok(()),
    }
}

/// Anything may run here (commands, files, web views): the bar's window
/// and its Stop key work (on Windows; elsewhere there is none). `Err` says
/// why not, for the chat; whatever runs is ended when this stops being so.
pub fn controls_ready() -> Result<(), String> {
    match not_ready() {
        Some(why) => Err(format!(
            "this computer's Stop bar isn't working ({why}), so agents can't do anything on this computer: the user must always be able to see and stop them. {RESTART}"
        )),
        None => Ok(()),
    }
}

const RESTART: &str = "Turning this computer off and on again as a machine there (This computer in the app), or restarting Tessera there, starts it again";

fn not_ready() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let b = bar();
    if b.window == 0 || b.key.is_none() {
        Some(
            b.failed
                .clone()
                .unwrap_or_else(|| "it hasn't started".into()),
        )
    } else if b.beat.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
        Some("it stopped answering".into())
    } else if b.desktop && !(b.placed && b.visible) {
        Some("it isn't on the screen".into())
    } else if b.desktop && !b.guarded {
        Some("it couldn't keep agents' input off Tessera's own windows".into())
    } else {
        None
    }
}

/// Before agents' input: the guard of Tessera's own windows still sees it
/// (Windows takes away a hook that answers too slowly, without a word). A
/// test move and key release of its own (swallowed while it works) must
/// reach it; else it is set up again and this action refused. Elsewhere:
/// nothing to look at.
pub fn guard_alive() -> Result<(), String> {
    imp::guard_alive()
}

/// For `tessera node status` and the app: the Stop key, or why there is no
/// bar. `None` elsewhere than Windows.
pub fn status() -> Option<String> {
    let b = bar();
    if !b.on || !cfg!(windows) {
        return None;
    }
    Some(match (&b.failed, b.key) {
        (None, Some(key)) if b.window != 0 => format!("Stop bar on; Stop key {key}"),
        (Some(why), _) => format!(
            "Stop bar couldn't start ({why}); desktop actions are refused{}",
            if b.desktop {
                ""
            } else {
                " (desktop control is off)"
            }
        ),
        _ => "Stop bar starting".into(),
    })
}

/// The bar's words, its Stop key and its button, as things are.
fn now_line() -> Line {
    let b = bar();
    let now = Instant::now();
    let doing: Vec<&Doing> = {
        let mut v: Vec<&(u64, Doing)> = b.doing.values().collect();
        v.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        v.into_iter().map(|(_, d)| d).collect()
    };
    let last = b
        .last
        .as_ref()
        .filter(|(_, at)| now.duration_since(*at) < RECENT)
        .map(|(d, at)| (d, now.duration_since(*at).as_secs()));
    let note = b
        .note
        .as_ref()
        .filter(|(_, at)| now.duration_since(*at) < Duration::from_secs(6))
        .map(|(n, _)| n.as_str());
    line(b.stopped, &doing, last, b.key, note)
}

/// How the bar looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// On, nothing under way.
    Calm,
    /// An agent is acting now.
    Busy,
    Stopped,
}

/// What the bar shows: its words (shortened to fit), the Stop key (drawn
/// apart, never cut), its button and look.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    text: String,
    key: Option<String>,
    button: &'static str,
    tone: Tone,
}

/// What is under way (the latest first), else the last chat a moment ago,
/// else that it is on.
fn line(
    stopped: bool,
    doing: &[&Doing],
    last: Option<(&Doing, u64)>,
    key: Option<&str>,
    note: Option<&str>,
) -> Line {
    let key_label = key.map(|k| format!("Stop: {k}"));
    let chat = |c: &str| {
        format!(
            "“{}”",
            crate::util::truncate(&crate::util::one_line(c, 60), 40)
        )
    };
    if stopped {
        let text = note.map(str::to_string).unwrap_or_else(|| {
            "Stopped. Agents can't use this computer until you press Resume.".into()
        });
        return Line {
            text,
            key: None,
            button: "Resume",
            tone: Tone::Stopped,
        };
    }
    let (text, tone) = match doing {
        [d, rest @ ..] => {
            let more = if rest.is_empty() {
                String::new()
            } else {
                format!(" (+{} more)", rest.len())
            };
            (
                format!(
                    "Tessera · {}: {}{more}",
                    chat(&d.chat),
                    crate::util::one_line(&d.what, 80)
                ),
                Tone::Busy,
            )
        }
        [] => match last {
            Some((d, secs)) => (
                format!(
                    "Tessera · {} used this computer {secs} s ago",
                    chat(&d.chat)
                ),
                Tone::Calm,
            ),
            None => ("Tessera desktop control is on".to_string(), Tone::Calm),
        },
    };
    Line {
        text: note.map(str::to_string).unwrap_or(text),
        key: key_label,
        button: "Stop",
        tone,
    }
}

/// The user pressed Stop (button or key): every desktop action ends at its
/// next step at once; the machine's loop does the rest (told first, so
/// commands end and the Stop is saved whatever letting go takes).
fn user_stop() {
    super::screen::halt();
    let events = {
        let mut b = bar();
        b.stopped = true;
        b.note = None;
        b.events.clone()
    };
    if let Some(tx) = events {
        let _ = tx.send(BarEvent::Stop);
    }
    // What an action holds right now (a drag's button, keys being sent),
    // from its own thread: the action may be stuck in Windows' call, and
    // so may this release.
    std::thread::spawn(super::screen::release_live);
}

/// Resume, with the user's own mouse (`real`); a click a program sent only
/// gets a word.
fn user_resume(real: bool) {
    let events = {
        let mut b = bar();
        if !real {
            b.note = Some((
                "Resume takes your own mouse (or This computer in the Tessera app, or `tessera node resume`).".into(),
                Instant::now(),
            ));
            return;
        }
        b.events.clone()
    };
    if let Some(tx) = events {
        let _ = tx.send(BarEvent::Resume);
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn start() {}
    pub fn refresh(_settings: bool) {}
    pub fn guard_alive() -> Result<(), String> {
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{
        ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreateSolidBrush, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS,
        DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect,
        GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_ESCAPE,
        VK_PAUSE,
    };
    use windows_sys::Win32::UI::Input::{
        GetCurrentInputMessageSource, IMO_HARDWARE, INPUT_MESSAGE_SOURCE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        LLMHF_INJECTED, MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
        WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
        WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WindowFromPoint,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
        HWND_TOPMOST, IDC_HAND, IsWindowVisible, LoadCursorW, MA_NOACTIVATE, MSG, PostMessageW,
        RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WM_APP, WM_CLOSE, WM_DISPLAYCHANGE,
        WM_DPICHANGED, WM_HOTKEY, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSEXW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    /// Show, hide, take or give back the key (the settings changed).
    const WM_SETTINGS: u32 = WM_APP + 1;
    /// Draw again (what is under way changed).
    const WM_DRAW: u32 = WM_APP + 2;
    /// Set the guard up again (it stopped seeing input).
    const WM_REGUARD: u32 = WM_APP + 3;
    const HOTKEY: i32 = 0x5453;
    const TICK: usize = 1;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const fn rgb(r: u8, g: u8, b: u8) -> u32 {
        r as u32 | (g as u32) << 8 | (b as u32) << 16
    }

    /// Start the bar's own thread; returns once its window is up (or
    /// failed), so [`ready`] answers right away.
    pub fn start() {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("tessera-stop-bar".into())
            .spawn(move || run(tx));
        if let Err(e) = spawned {
            bar().failed = Some(format!("couldn't start its thread: {e}"));
            return;
        }
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }

    /// Tell the bar's thread: `settings` changed (show/hide, key), or only
    /// what it says.
    pub fn refresh(settings: bool) {
        let window = bar().window;
        if window != 0 {
            // SAFETY: posting to our own window (a stale one just fails).
            unsafe {
                PostMessageW(
                    window as HWND,
                    if settings { WM_SETTINGS } else { WM_DRAW },
                    0,
                    0,
                )
            };
        }
    }

    fn run(up: std::sync::mpsc::Sender<()>) {
        // SAFETY: Win32 calls on this thread's own window; the class and
        // title strings outlive the calls.
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("TesseraStopBar");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            // Already there from an earlier try: that one is fine.
            if RegisterClassExW(&wc) == 0
                && std::io::Error::last_os_error().raw_os_error()
                    != Some(ERROR_CLASS_ALREADY_EXISTS as i32)
            {
                bar().failed = Some(format!(
                    "Windows refused its window ({})",
                    std::io::Error::last_os_error()
                ));
                let _ = up.send(());
                return;
            }
            let title = wide("Tessera Stop bar");
            let window = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                10,
                10,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                bar().failed = Some(format!(
                    "Windows refused its window ({})",
                    std::io::Error::last_os_error()
                ));
                let _ = up.send(());
                return;
            }
            {
                let mut b = bar();
                b.window = window as isize;
                b.beat = Some(Instant::now());
            }
            SetTimer(window, TICK, 500, None);
            settings(window);
            let _ = up.send(());
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // Gone: said, and started again by the next `sync`.
            let mut b = bar();
            b.window = 0;
            b.key = None;
            // Its hooks ended with its thread.
            b.guarded = false;
            b.failed = Some("its window closed".into());
        }
    }

    thread_local! {
        /// The guard's mouse and keyboard hooks (this thread's own).
        static GUARD: std::cell::Cell<[HHOOK; 2]> =
            const { std::cell::Cell::new([std::ptr::null_mut(); 2]) };
    }

    /// Test input the guard's hooks saw (see [`guard_alive`]).
    static MOUSE_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static KEYS_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    pub fn guard_alive() -> Result<(), String> {
        use std::sync::atomic::Ordering::SeqCst;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
            MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
        };
        let (mouse, keys) = (MOUSE_SEEN.load(SeqCst), KEYS_SEEN.load(SeqCst));
        // A move by nothing and the release of a key code Windows leaves
        // unassigned (0x0E): swallowed by the hooks while they work; if
        // they're gone, a program gets them (rarely noticeable, never a
        // press), and this action is refused.
        let probes = [
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE,
                        time: 0,
                        dwExtraInfo: super::super::screen::PROBE_MARK,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0x0E,
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: super::super::screen::PROBE_MARK,
                    },
                },
            },
        ];
        // SAFETY: two initialized INPUTs of the size given.
        let sent = unsafe {
            SendInput(
                probes.len() as u32,
                probes.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        let until = Instant::now() + Duration::from_millis(500);
        while sent == probes.len() as u32 && Instant::now() < until {
            if MOUSE_SEEN.load(SeqCst) > mouse && KEYS_SEEN.load(SeqCst) > keys {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Gone (or Windows took no input): set up again, and not this time.
        let window = {
            let mut b = bar();
            b.guarded = false;
            b.window
        };
        if window != 0 {
            // SAFETY: posting to our own window (fine if it just closed).
            unsafe {
                PostMessageW(window as HWND, WM_REGUARD, 0, 0);
            }
        }
        Err("Tessera's guard that keeps agents' clicks and keys off its own windows stopped working; it is being set up again, so this action didn't run: try again in a moment".into())
    }

    /// Clicks, wheel turns and key presses Tessera's desktop actions send
    /// (marked as such) never reach Tessera's own windows: refused where
    /// they arrive, whatever moved or took the focus since the action
    /// looked. Releases always pass (a key or button never stays down);
    /// anything else (the user's own input, other programs') is untouched.
    unsafe extern "system" fn mouse_guard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: Windows passes a valid MSLLHOOKSTRUCT for HC_ACTION.
        unsafe {
            if code == HC_ACTION as i32 {
                let m = &*(lparam as *const MSLLHOOKSTRUCT);
                if m.flags & LLMHF_INJECTED != 0
                    && m.dwExtraInfo == super::super::screen::PROBE_MARK
                {
                    MOUSE_SEEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return 1;
                }
                // While no input of theirs may land (stopped, or an action
                // asked to stop still going), not even a move.
                if m.flags & LLMHF_INJECTED != 0
                    && m.dwExtraInfo == super::super::screen::INPUT_MARK
                    && wparam as u32 == WM_MOUSEMOVE
                    && super::super::screen::input_refused()
                {
                    return 1;
                }
                if m.flags & LLMHF_INJECTED != 0
                    && m.dwExtraInfo == super::super::screen::INPUT_MARK
                    && matches!(
                        wparam as u32,
                        WM_LBUTTONDOWN
                            | WM_RBUTTONDOWN
                            | WM_MBUTTONDOWN
                            | WM_XBUTTONDOWN
                            | WM_MOUSEWHEEL
                            | WM_MOUSEHWHEEL
                    )
                    && (super::super::screen::input_refused()
                        || super::super::screen::tessera_window(WindowFromPoint(m.pt)))
                {
                    return 1;
                }
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }

    unsafe extern "system" fn key_guard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: Windows passes a valid KBDLLHOOKSTRUCT for HC_ACTION.
        unsafe {
            if code == HC_ACTION as i32 {
                let k = &*(lparam as *const KBDLLHOOKSTRUCT);
                if k.flags & LLKHF_INJECTED != 0
                    && k.dwExtraInfo == super::super::screen::PROBE_MARK
                {
                    KEYS_SEEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return 1;
                }
                if k.flags & LLKHF_INJECTED != 0
                    && k.dwExtraInfo == super::super::screen::INPUT_MARK
                    && matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN)
                    && (super::super::screen::input_refused()
                        || super::super::screen::tessera_window(GetForegroundWindow()))
                {
                    return 1;
                }
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
    }

    /// The guard in place (with desktop control) or taken away; `guarded`
    /// says whether it is, and desktop actions wait for it.
    unsafe fn guard(on: bool) {
        // SAFETY: hooks of this thread, set and removed on it.
        unsafe {
            let [mouse, keys] = GUARD.with(|g| g.get());
            if on && (mouse.is_null() || keys.is_null()) {
                let module = GetModuleHandleW(std::ptr::null());
                let mouse = if mouse.is_null() {
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_guard), module, 0)
                } else {
                    mouse
                };
                let keys = if keys.is_null() {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_guard), module, 0)
                } else {
                    keys
                };
                GUARD.with(|g| g.set([mouse, keys]));
            } else if !on {
                for h in [mouse, keys] {
                    if !h.is_null() {
                        UnhookWindowsHookEx(h);
                    }
                }
                GUARD.with(|g| g.set([std::ptr::null_mut(); 2]));
            }
            let [mouse, keys] = GUARD.with(|g| g.get());
            bar().guarded = on && !mouse.is_null() && !keys.is_null();
        }
    }

    /// Show it with its key while desktop control is on; else hide it and
    /// give the key back.
    unsafe fn settings(window: HWND) {
        let (on, desktop, key, shown) = {
            let b = bar();
            (b.on, b.desktop, b.key, b.shown(Instant::now()))
        };
        // SAFETY: our own window, on its thread.
        unsafe {
            guard(on && desktop);
            if on {
                if key.is_none() {
                    let keys = [
                        (MOD_CONTROL | MOD_ALT, u32::from(VK_ESCAPE)),
                        (MOD_CONTROL | MOD_ALT, u32::from(VK_PAUSE)),
                        (MOD_CONTROL | MOD_ALT | MOD_SHIFT, u32::from(b'S')),
                    ];
                    let got = keys.iter().position(|(mods, vk)| {
                        RegisterHotKey(window, HOTKEY, mods | MOD_NOREPEAT, *vk) != 0
                    });
                    let mut b = bar();
                    match got {
                        Some(i) => {
                            b.key = Some(KEYS[i]);
                            b.failed = None;
                        }
                        None => {
                            b.failed = Some(format!(
                                "other programs hold every Stop key it tries: {}",
                                KEYS.join(", ")
                            ))
                        }
                    }
                }
                show(window, shown);
            } else {
                if key.is_some() {
                    UnregisterHotKey(window, HOTKEY);
                    bar().key = None;
                }
                ShowWindow(window, SW_HIDE);
            }
            InvalidateRect(window, std::ptr::null(), 1);
        }
    }

    /// On screen (above everything) or not.
    unsafe fn show(window: HWND, shown: bool) {
        // SAFETY: our own window, on its thread.
        let visible = unsafe {
            if shown {
                if IsWindowVisible(window) == 0 {
                    place(window);
                    ShowWindow(window, SW_SHOWNOACTIVATE);
                }
            } else if IsWindowVisible(window) != 0 {
                ShowWindow(window, SW_HIDE);
            }
            IsWindowVisible(window) != 0
        };
        bar().visible = visible;
    }

    fn scaled(window: HWND, v: i32) -> i32 {
        // SAFETY: plain call on our own window.
        let dpi = unsafe { GetDpiForWindow(window) }.max(96) as i32;
        v * dpi / 96
    }

    /// Top middle of the main screen, above everything.
    unsafe fn place(window: HWND) {
        // SAFETY: plain calls; `mi` is sized for the call.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            let placed = GetMonitorInfoW(monitor, &mut mi) != 0 && {
                let work = mi.rcWork;
                let room = work.right - work.left;
                let w = scaled(window, 620).min(room - scaled(window, 16)).max(200);
                let h = scaled(window, 34);
                let x = work.left + (room - w) / 2;
                SetWindowPos(window, HWND_TOPMOST, x, work.top, w, h, SWP_NOACTIVATE) != 0
            };
            bar().placed = placed;
        }
    }

    /// The button's place in the bar.
    fn button(window: HWND, client: &RECT) -> RECT {
        let pad = scaled(window, 4);
        RECT {
            left: client.right - scaled(window, 96),
            top: client.top + pad,
            right: client.right - pad,
            bottom: client.bottom - pad,
        }
    }

    unsafe fn paint(window: HWND) {
        // What this paint shows: up to this action (see [`super::shown`]).
        let painted = bar().seq;
        let Line {
            text,
            key,
            button: label,
            tone,
        } = now_line();
        // SAFETY: painting our own window between BeginPaint and EndPaint;
        // every object made here is deleted.
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(window, &mut ps);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(window, &mut client);
            let back = match tone {
                Tone::Calm => rgb(38, 38, 44),
                Tone::Busy => rgb(150, 82, 0),
                Tone::Stopped => rgb(150, 24, 24),
            };
            let brush = CreateSolidBrush(back);
            FillRect(dc, &client, brush);
            DeleteObject(brush);
            let face = wide("Segoe UI");
            // Weight 600, default character set, ClearType.
            let font = CreateFontW(
                -scaled(window, 15),
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                face.as_ptr(),
            );
            let old = SelectObject(dc, font);
            SetBkMode(dc, TRANSPARENT as i32);
            let btn = button(window, &client);
            let white = CreateSolidBrush(rgb(255, 255, 255));
            FillRect(dc, &btn, white);
            DeleteObject(white);
            SetTextColor(dc, back);
            let mut l = wide(label);
            let mut r = btn;
            DrawTextW(
                dc,
                l.as_mut_ptr(),
                -1,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX,
            );
            SetTextColor(dc, rgb(255, 255, 255));
            // The Stop key first, at its full width, just left of the button.
            let mut right = btn.left - scaled(window, 8);
            if let Some(k) = key {
                let mut k = wide(&k);
                let mut need = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                DrawTextW(
                    dc,
                    k.as_mut_ptr(),
                    -1,
                    &mut need,
                    DT_SINGLELINE | DT_NOPREFIX | DT_CALCRECT,
                );
                let mut r = RECT {
                    left: right - (need.right - need.left),
                    top: client.top,
                    right,
                    bottom: client.bottom,
                };
                DrawTextW(
                    dc,
                    k.as_mut_ptr(),
                    -1,
                    &mut r,
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
                right = r.left - scaled(window, 12);
            }
            let mut t = wide(&text);
            let mut r = RECT {
                left: client.left + scaled(window, 12),
                top: client.top,
                right,
                bottom: client.bottom,
            };
            DrawTextW(
                dc,
                t.as_mut_ptr(),
                -1,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(dc, old);
            DeleteObject(font);
            EndPaint(window, &ps);
            // On screen now, saying what was under way when it was drawn.
            let mut b = bar();
            if b.visible && b.placed {
                b.drawn = b.drawn.max(painted);
            }
        }
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: called by Windows for our own window, on its thread.
        unsafe {
            match msg {
                WM_PAINT => {
                    paint(window);
                    0
                }
                WM_TIMER => {
                    // Alive. Shown or hidden as agents work, above whatever
                    // came up since; the "s ago" moves on.
                    let shown = {
                        let mut b = bar();
                        b.beat = Some(Instant::now());
                        b.shown(Instant::now())
                    };
                    show(window, shown);
                    if shown {
                        SetWindowPos(
                            window,
                            HWND_TOPMOST,
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                    InvalidateRect(window, std::ptr::null(), 0);
                    0
                }
                WM_SETTINGS => {
                    settings(window);
                    0
                }
                WM_DRAW => {
                    let shown = bar().shown(Instant::now());
                    show(window, shown);
                    InvalidateRect(window, std::ptr::null(), 0);
                    0
                }
                WM_REGUARD => {
                    let (on, desktop) = {
                        let b = bar();
                        (b.on, b.desktop)
                    };
                    guard(false);
                    guard(on && desktop);
                    0
                }
                WM_HOTKEY => {
                    if !bar().stopped {
                        user_stop();
                    }
                    InvalidateRect(window, std::ptr::null(), 0);
                    0
                }
                // Clicking it never takes the keyboard from the user's program.
                WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
                WM_LBUTTONUP => {
                    let x = (lparam & 0xFFFF) as u16 as i16 as i32;
                    let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                    let stopped = bar().stopped;
                    if !stopped {
                        // Anywhere on the bar: the bigger the better.
                        user_stop();
                    } else {
                        let mut client: RECT = std::mem::zeroed();
                        GetClientRect(window, &mut client);
                        let b = button(window, &client);
                        if x >= b.left && x < b.right && y >= b.top && y < b.bottom {
                            // Only the user's own mouse: not a click a program sent.
                            let mut source: INPUT_MESSAGE_SOURCE = std::mem::zeroed();
                            let real = GetCurrentInputMessageSource(&mut source) != 0
                                && source.originId == IMO_HARDWARE;
                            user_resume(real);
                        }
                    }
                    InvalidateRect(window, std::ptr::null(), 0);
                    0
                }
                WM_DISPLAYCHANGE | WM_DPICHANGED => {
                    place(window);
                    InvalidateRect(window, std::ptr::null(), 1);
                    0
                }
                // It stays while desktop control is on.
                WM_CLOSE => 0,
                _ => DefWindowProcW(window, msg, wparam, lparam),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doing(chat: &str, what: &str) -> Doing {
        Doing {
            chat: chat.into(),
            what: what.into(),
        }
    }

    #[test]
    fn the_bar_says_who_is_doing_what_and_how_to_stop_it() {
        let key = Some("Ctrl+Alt+Esc");
        let l = line(false, &[], None, key, None);
        assert_eq!(l.text, "Tessera desktop control is on");
        // The key is drawn apart: never shortened away with the words.
        assert_eq!(l.key.as_deref(), Some("Stop: Ctrl+Alt+Esc"));
        assert_eq!((l.button, l.tone), ("Stop", Tone::Calm));
        let a = doing("Fix the tests", "click at 640,400");
        let c = doing("Write the docs", "command cargo …");
        let l = line(false, &[&a], None, key, None);
        assert_eq!(l.text, "Tessera · “Fix the tests”: click at 640,400");
        assert_eq!((l.tone, l.key.is_some()), (Tone::Busy, true));
        // The latest first, and how many more.
        let l = line(false, &[&c, &a], None, key, None);
        assert_eq!(
            l.text,
            "Tessera · “Write the docs”: command cargo … (+1 more)"
        );
        // A moment after: who it was.
        let l = line(false, &[], Some((&a, 12)), key, None);
        assert_eq!(
            l.text,
            "Tessera · “Fix the tests” used this computer 12 s ago"
        );
        assert_eq!(l.tone, Tone::Calm);
        // Stopped: one way on.
        let l = line(true, &[&a], None, key, None);
        assert_eq!(
            l.text,
            "Stopped. Agents can't use this computer until you press Resume."
        );
        assert_eq!((l.button, l.tone), ("Resume", Tone::Stopped));
        // A long chat name is shortened; lines never break the bar.
        let long = doing(&"x".repeat(200), "type 12 characters\nnext");
        let l = line(false, &[&long], None, key, None);
        assert!(
            !l.text.contains('\n') && l.text.chars().count() < 160,
            "{}",
            l.text
        );
    }

    #[test]
    fn the_bar_shows_with_desktop_control_or_while_agents_work() {
        let now = Instant::now();
        let mut b = Shared {
            on: true,
            ..Default::default()
        };
        assert!(!b.shown(now), "nothing under way, no desktop control");
        b.doing
            .insert("c1".into(), (1, doing("Chat", "command dir")));
        assert!(b.shown(now));
        let (_, d) = b.doing.remove("c1").unwrap();
        b.last = Some((d, now));
        assert!(b.shown(now + Duration::from_secs(30)), "a minute after");
        assert!(!b.shown(now + RECENT + Duration::from_secs(1)));
        b.desktop = true;
        assert!(b.shown(now + RECENT * 2));
        b.desktop = false;
        b.stopped = true;
        assert!(b.shown(now + RECENT * 2), "stopped: until resumed");
        b.on = false;
        assert!(!b.shown(now));
    }

    #[test]
    fn a_resume_a_program_sent_only_gets_a_word() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        listen(tx);
        user_resume(false);
        assert!(rx.try_recv().is_err());
        assert!(
            bar()
                .note
                .as_ref()
                .is_some_and(|(n, _)| n.contains("own mouse"))
        );
        user_resume(true);
        assert_eq!(rx.try_recv().ok(), Some(BarEvent::Resume));
    }
}
