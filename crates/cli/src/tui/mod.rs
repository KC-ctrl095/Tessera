//! The terminal app: Tessera as a lean coding harness. One chat fills the
//! window (`/split` puts a few side by side); everything else is a slash
//! command. Chats are the server's, shared with the apps.
//!
//! State lives in [`app::App`] (one [`pane::Pane`] per chat on screen);
//! [`ui`] draws it. Backend calls run on spawned tasks and hand their
//! results back to the UI loop as closures, so the loop never blocks on the
//! engine or the network.

mod app;
mod ask;
mod commands;
mod editor;
mod markdown;
mod motif;
mod pane;
mod settings;
pub(crate) mod text;
mod theme;
mod transcript;
mod ui;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute, queue};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::backend::{Backend, BackendEvent};

pub struct TuiOptions {
    pub prompt: Option<String>,
    pub resume: Option<String>,
    pub continue_last: bool,
    pub model: Option<String>,
    pub plain: bool,
    /// New chats start in YOLO mode (nothing asks for approval).
    pub yolo: bool,
    /// Where interface preferences (mouse capture, thinking visibility,
    /// theme, permissions for new chats) persist.
    pub prefs_file: std::path::PathBuf,
    /// Stray stderr output (an in-process engine, child processes) lands
    /// here while the TUI owns the screen.
    pub log_file: std::path::PathBuf,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The real stderr while the TUI owns the screen (see `quiet_stderr`).
static SAVED_STDERR: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
/// Where stray output goes while the TUI runs.
static STDERR_LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// While the TUI draws, anything written to stderr — an in-process
/// engine's or a child process's (a browser, a tool) — would scribble over
/// the screen. Send it to a log file next to the preferences instead.
#[cfg(unix)]
fn quiet_stderr() {
    use std::os::fd::AsRawFd;
    if SAVED_STDERR.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let Some(path) = STDERR_LOG.get() else { return };
    // Keep it small: start over past 1 MB.
    let big = std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20);
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(!big)
        .write(true)
        .truncate(big)
        .open(path)
    else {
        return;
    };
    // SAFETY: plain descriptor duplication; `f` stays open until dup2 ran.
    unsafe {
        let saved = libc::dup(2);
        if saved >= 0 {
            if libc::dup2(f.as_raw_fd(), 2) >= 0 {
                SAVED_STDERR.store(saved, Ordering::SeqCst);
            } else {
                libc::close(saved);
            }
        }
    }
}

/// Windows: the same, through the process's standard handle.
#[cfg(windows)]
fn quiet_stderr() {
    use std::sync::Mutex;
    static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
    if SAVED_STDERR.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let Some(path) = STDERR_LOG.get() else { return };
    let big = std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20);
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(!big)
        .write(true)
        .truncate(big)
        .open(path)
    else {
        return;
    };
    if let Some(old) = tessera_core::platform::windows::redirect_stderr(&f) {
        WIN_SAVED.store(old, Ordering::SeqCst);
        SAVED_STDERR.store(0, Ordering::SeqCst);
        if let Ok(mut keep) = FILE.lock() {
            *keep = Some(f);
        }
    }
}

/// The standard error handle `quiet_stderr` replaced (Windows).
#[cfg(windows)]
static WIN_SAVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(not(any(unix, windows)))]
fn quiet_stderr() {}

/// Give stderr back (before leaving, and before a panic message).
fn restore_stderr() {
    let saved = SAVED_STDERR.swap(-1, Ordering::SeqCst);
    #[cfg(unix)]
    if saved >= 0 {
        // SAFETY: `saved` is the descriptor `quiet_stderr` duplicated.
        unsafe {
            libc::dup2(saved, 2);
            libc::close(saved);
        }
    }
    #[cfg(windows)]
    if saved >= 0 {
        tessera_core::platform::windows::restore_stderr(WIN_SAVED.load(Ordering::SeqCst));
    }
    #[cfg(not(any(unix, windows)))]
    let _ = saved;
}
static KBD: AtomicBool = AtomicBool::new(false);
static MOUSE: AtomicBool = AtomicBool::new(true);

const FRAME: Duration = Duration::from_millis(33);

fn enter_terminal(query_keyboard: bool) -> io::Result<()> {
    quiet_stderr();
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    let mut out = io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        cursor::Hide
    )?;
    // Without mouse capture many terminals turn the wheel into ↑/↓ in the
    // alternate screen, which would scroll prompt history. Disable that.
    let _ = out.write_all(b"\x1b[?1007l");
    if query_keyboard {
        let ok = matches!(
            crossterm::terminal::supports_keyboard_enhancement(),
            Ok(true)
        );
        KBD.store(ok, Ordering::SeqCst);
    }
    if KBD.load(Ordering::SeqCst) {
        // Lets Shift+Enter / Ctrl+Enter arrive as distinct keys.
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    if MOUSE.load(Ordering::SeqCst) {
        let _ = execute!(out, EnableMouseCapture);
    }
    out.flush()
}

/// Idempotent; safe to call from the panic hook.
fn restore_terminal() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    if KBD.load(Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE.load(Ordering::SeqCst) {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = out.write_all(b"\x1b[?1007h");
    let _ = out.flush();
    let _ = disable_raw_mode();
    restore_stderr();
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev(info);
    }));
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn suspend(term: &mut Term) -> io::Result<()> {
    restore_terminal();
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-TSTP")
            .arg(std::process::id().to_string())
            .status();
    }
    enter_terminal(false)?;
    term.clear()
}

/// Ask the terminal for its background colour (OSC 11), before anything
/// else reads the input. The device-attributes query that follows is
/// answered by every terminal, so we never wait long for one that doesn't
/// know OSC 11. Raw mode must be on.
#[cfg(unix)]
fn query_tone() -> Option<theme::Tone> {
    use std::io::IsTerminal;
    use std::os::fd::AsRawFd;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    let mut out = io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let fd = io::stdin().as_raw_fd();
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if r <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: reads at most `chunk.len()` bytes into `chunk`.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        // The device-attributes answer (`ESC [ ? … c`) comes last.
        if let Some(i) = buf.windows(3).position(|w| w == b"\x1b[?")
            && buf[i..].contains(&b'c')
        {
            break;
        }
    }
    theme::tone_from_osc(&buf)
}

#[cfg(not(unix))]
fn query_tone() -> Option<theme::Tone> {
    None
}

/// What the terminal's background looks like (asked once per process).
fn terminal_tone() -> Option<theme::Tone> {
    static TONE: std::sync::OnceLock<Option<theme::Tone>> = std::sync::OnceLock::new();
    *TONE.get_or_init(|| {
        let colour = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true);
        if !colour {
            return None;
        }
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if !was_raw {
            let _ = enable_raw_mode();
        }
        let t = query_tone();
        if !was_raw {
            let _ = disable_raw_mode();
        }
        t.or_else(|| {
            std::env::var("COLORFGBG")
                .ok()
                .and_then(|v| theme::tone_from_colorfgbg(&v))
        })
    })
}

/// The terminal has a light background (a dark or light `TESSERA_THEME`
/// wins). Used by the setup wizard to pick its tones.
pub(crate) fn light_terminal() -> bool {
    match std::env::var("TESSERA_THEME")
        .ok()
        .and_then(|v| theme::ThemePref::parse(&v))
        .and_then(|p| p.fixed_tone())
    {
        Some(tone) => tone == theme::Tone::Light,
        None => terminal_tone() == Some(theme::Tone::Light),
    }
}

pub async fn run(backend: Backend, opts: TuiOptions) -> Result<()> {
    let prefs = app::Prefs::load(&opts.prefs_file);
    let pref = prefs
        .theme
        .as_deref()
        .and_then(theme::ThemePref::parse)
        .unwrap_or_default();
    // The terminal's background decides dark or light (unless chosen).
    let tone = if opts.plain { None } else { terminal_tone() };
    let theme = theme::Theme::detect(opts.plain, pref, tone);
    let mut events = backend.subscribe();
    let (tx, mut rx) = mpsc::unbounded_channel::<app::Job>();
    let mut app = app::App::new(backend, theme, opts.plain, tx);
    app.mouse = prefs.mouse;
    app.show_thinking = prefs.show_thinking;
    app.new_permission = prefs.permission.as_deref().and_then(app::parse_permission);
    app.prefs_file = Some(opts.prefs_file.clone());
    MOUSE.store(app.mouse, Ordering::SeqCst);
    install_panic_hook();
    let _ = STDERR_LOG.set(opts.log_file.clone());
    enter_terminal(true)?;
    app.kbd_enhanced = KBD.load(Ordering::SeqCst);
    let result = event_loop(&mut app, &mut events, &mut rx, &opts).await;
    restore_terminal();
    let dim = if theme.has_color() { "\x1b[2m" } else { "" };
    let reset = if theme.has_color() { "\x1b[0m" } else { "" };
    if let Some(s) = app.pane().session.as_ref().filter(|s| s.last_seq > 0) {
        println!(
            "{dim}“{}” · continue with tessera here, or tessera -r {}{reset}",
            text::truncate(&s.title, 40),
            s.id
        );
    }
    app.shutdown().await;
    result
}

async fn event_loop(
    app: &mut app::App,
    events: &mut mpsc::UnboundedReceiver<BackendEvent>,
    rx: &mut mpsc::UnboundedReceiver<app::Job>,
    opts: &TuiOptions,
) -> Result<()> {
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    term.clear()?;
    app.start(opts);
    let mut input = EventStream::new();
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut last_draw = Instant::now() - FRAME;
    // The next frame repaints the whole screen (not a fair speed sample).
    let mut repainted = true;
    let mut events_open = true;
    let mut input_open = true;
    // A full repaint waits for the next frame so the clear and the new
    // picture reach the terminal together.
    let mut clear = false;
    loop {
        if app.force_clear {
            app.force_clear = false;
            clear = true;
            app.dirty = true;
        }
        if app.dirty && last_draw.elapsed() >= FRAME {
            app.dirty = false;
            let t0 = Instant::now();
            // Terminals that know synchronized output (kitty, WezTerm,
            // foot, Ghostty, iTerm2, recent tmux…) show the frame at once,
            // with no tearing; others ignore it.
            let mut out = io::stdout();
            let _ = queue!(out, BeginSynchronizedUpdate);
            if clear {
                clear = false;
                term.clear()?;
                repainted = true;
            }
            term.draw(|f| ui::draw(f, app))?;
            if !repainted {
                app.note_frame(t0.elapsed());
            }
            repainted = false;
            let _ = execute!(out, EndSynchronizedUpdate);
            last_draw = Instant::now();
        }
        let mut deadline = app.next_deadline();
        if app.dirty {
            let d = last_draw + FRAME;
            deadline = Some(deadline.map_or(d, |x| x.min(d)));
        }
        let sleep = async move {
            match deadline {
                Some(d) => tokio::time::sleep_until(d.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        #[cfg(unix)]
        let signals = async {
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sighup.recv() => {}
            }
        };
        #[cfg(not(unix))]
        let signals = std::future::pending::<()>();
        tokio::select! {
            ev = input.next(), if input_open => match ev {
                Some(Ok(ev)) => app.on_terminal(ev),
                Some(Err(_)) | None => {
                    input_open = false;
                    app.quit = true;
                }
            },
            ev = events.recv(), if events_open => match ev {
                Some(ev) => {
                    app.on_backend(ev);
                    // Coalesce bursts (stream deltas) into one frame.
                    for _ in 0..2000 {
                        match events.try_recv() {
                            Ok(ev) => app.on_backend(ev),
                            Err(_) => break,
                        }
                    }
                }
                None => {
                    events_open = false;
                    app.error("Lost the event stream from the engine; restart tessera to reconnect.");
                }
            },
            Some(job) = rx.recv() => {
                job(app);
                while let Ok(job) = rx.try_recv() {
                    job(app);
                }
            }
            _ = sleep => {
                app.tick();
                if app.animating() {
                    app.dirty = true;
                }
            }
            _ = signals => app.quit = true,
        }
        if app.mouse_changed {
            app.mouse_changed = false;
            MOUSE.store(app.mouse, Ordering::SeqCst);
            let mut out = io::stdout();
            let _ = if app.mouse {
                execute!(out, EnableMouseCapture)
            } else {
                execute!(out, DisableMouseCapture)
            };
        }
        if app.suspend {
            app.suspend = false;
            suspend(&mut term)?;
            app.dirty = true;
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}
