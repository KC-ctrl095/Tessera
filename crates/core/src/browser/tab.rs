//! One tab: its page target, the devices watching it, and what they send.
//!
//! Frames flow to each device within a window: one at a time, or for
//! devices that take several (`pipelined`) as many as their connection
//! carries without queueing (the window grows while acknowledgements come
//! back as fast as the quickest ones did, and shrinks when they slow down).
//! The newest frame replaces any that wasn't sent yet. The browser's own
//! screencast ack is held while every device is busy, so pages are painted
//! only as fast as the quickest device takes them, and a static page costs
//! nothing.
//!
//! While the page moves (scrolling, animations), frames get smaller on
//! connections that can't keep up (`LADDER`); between movements (a click,
//! typing) they are full size. Once it stops moving, devices that draw WebP
//! get one sharp picture at their full pixel ratio.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_proto::{
    BrowserDialogKind, BrowserInputEvent, BrowserKeyKind, BrowserMouseButton, BrowserMouseKind,
    BrowserNavAction, BrowserTab, BrowserTouchKind, Event, NoticeLevel,
};
use tokio::sync::{mpsc, watch};

use super::cdp::{Cdp, CdpEvent};
use super::{Browser, Live, Shared, keys};

/// The isolated world Tessera's page scripts run in.
pub(crate) const WORLD: &str = "tessera";
/// Reports focus changes so phones can raise their keyboard.
const FOCUS_JS: &str = r#"(() => {
  if (globalThis.__tsrFocusOn) return;
  globalThis.__tsrFocusOn = true;
  const kind = (el) => {
    if (!el) return '';
    if (el.isContentEditable) return 'contenteditable';
    if (el.tagName === 'TEXTAREA') return el.readOnly || el.disabled ? '' : 'textarea';
    if (el.tagName === 'INPUT') {
      const t = (el.type || 'text').toLowerCase();
      if (['button','submit','reset','checkbox','radio','file','image','range','color','hidden'].includes(t)) return '';
      return el.readOnly || el.disabled ? '' : t;
    }
    return '';
  };
  let last = null;
  const check = () => {
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    if (el && (el.tagName === 'IFRAME' || el.tagName === 'FRAME')) return; // the frame reports
    if (window !== top && !document.hasFocus()) return;
    const k = kind(el);
    if (k === last) return;
    last = k;
    try { __tsrFocus(k); } catch (e) {}
  };
  addEventListener('focusin', check, true);
  addEventListener('focusout', () => setTimeout(check, 0), true);
  if (window !== top) return;
  // Title changes (Chrome doesn't report them to the debugger).
  let title = null, timer = 0;
  const report = () => { timer = 0; if (document.title !== title) { title = document.title; try { __tsrTitle(title); } catch (e) {} } };
  const watch = () => {
    const root = document.head || document.documentElement;
    if (!root) return;
    new MutationObserver(() => { if (!timer) timer = setTimeout(report, 50); })
      .observe(root, { childList: true, subtree: true, characterData: true });
    report();
  };
  if (document.readyState === 'loading') addEventListener('DOMContentLoaded', watch, { once: true }); else watch();
  // A site's "prove you're a person" check: a hint for the tile (it offers
  // to finish it elsewhere), nothing more. Looked at as the page loads.
  const FRAMES = 'iframe[src*="challenges.cloudflare.com"],iframe[src*="/recaptcha/"],iframe[src*="recaptcha.net"],iframe[src*="hcaptcha.com"],iframe[src*="arkoselabs.com"],iframe[src*="funcaptcha.com"],iframe[src*="captcha-delivery.com"]';
  const WORDS = /verification challenge|verify (that )?you(\u2019|')?re (a )?human|verify you are (a )?human|are you a (robot|human)|not a robot|complete the security check|checking your browser|press and hold/i;
  let isCheck = false;
  const look = () => {
    let now = false;
    try { now = !!document.querySelector(FRAMES) || WORDS.test(((document.body && document.body.innerText) || '').slice(0, 20000)); } catch (e) {}
    // Reported once it got through (the binding comes when the tile watches).
    if (now !== isCheck) { try { __tsrCheck(now ? '1' : ''); isCheck = now; } catch (e) {} }
  };
  // Asked again when the tile starts watching a page that already loaded.
  globalThis.__tsrLook = () => { isCheck = null; look(); };
  const looks = () => { look(); setTimeout(look, 2000); setTimeout(look, 6000); };
  if (document.readyState === 'loading') addEventListener('DOMContentLoaded', looks, { once: true }); else looks();
  // Many sign-in pages ask only after an attempt, without leaving the page:
  // looked at again a little after a click, Enter or a form sent.
  let again = 0;
  const later = () => { clearTimeout(again); again = setTimeout(() => { look(); again = setTimeout(look, 3500); }, 1500); };
  addEventListener('submit', later, true);
  addEventListener('click', later, true);
  addEventListener('keydown', (e) => { if (e.key === 'Enter') later(); }, true);
})();"#;

/// Web views have no security keys or passkeys (the browser starts without
/// WebAuthn, see `process::args`), but Chromium still starts a request
/// when a site asks without checking first, and then waits for a key that
/// nobody can plug in. In the page's own world, before its scripts run:
/// such requests are turned down at once, as when someone cancels, so the
/// site offers its other ways to sign in. Other credentials (saved
/// passwords, federated sign-in) are untouched.
pub(crate) const NO_KEYS_JS: &str = r#"(() => {
  const C = globalThis.CredentialsContainer;
  if (!C || Object.prototype.hasOwnProperty.call(C.prototype, '__tsrNoKeys')) return;
  Object.defineProperty(C.prototype, '__tsrNoKeys', { value: true });
  const refuse = () => Promise.reject(new DOMException('Security keys and passkeys are not available in this browser. Choose another way to sign in.', 'NotAllowedError'));
  for (const name of ['get', 'create']) {
    const real = C.prototype[name];
    if (typeof real !== 'function') continue;
    Object.defineProperty(C.prototype, name, {
      configurable: true,
      writable: true,
      value: { [name](options) { return options && options.publicKey ? refuse() : real.call(this, options); } }[name],
    });
  }
})();"#;

/// Smallest and largest tile a device may attach with (CSS pixels).
const MIN_W: u32 = 200;
const MIN_H: u32 = 150;
const MAX_W: u32 = 3840;
const MAX_H: u32 = 2400;
/// Frames are at most this many device pixels on the long side, and about
/// this many in all (CSS size × dpr, reduced to fit).
const MAX_SIDE: f64 = 2048.0;
const MAX_PIXELS: f64 = 3.2e6;
/// A frame not acknowledged within this long is taken as lost.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// How fast a connection carries frames is the best seen this lately.
const RATE_SPAN: Duration = Duration::from_secs(3);
/// Popups stacked on one tab at most (more are closed).
const MAX_POPUPS: usize = 4;
/// Bytes on their way to one device (`pipelined` devices): to start with,
/// at least and at most; and frames at most.
const START_WINDOW: usize = 128 << 10;
const MIN_WINDOW: usize = 64 << 10;
const MAX_WINDOW: usize = 4 << 20;
const MAX_IN_FLIGHT: usize = 8;
/// Browser frames left unacknowledged while every device is busy: the
/// browser runs at most three ahead, so it paints just one more.
const PARKED: usize = 2;
/// While the page moves: (share of the full frame size, JPEG quality),
/// sharpest first. A connection that can't carry enough frames a second
/// steps down; one with room to spare steps up again.
const LADDER: [(f64, u8); 6] = [
    (1.0, 80),
    (1.0, 65),
    (1.0, 50),
    (0.75, 50),
    (0.5, 50),
    (0.5, 35),
];
/// About how big frames of each step are, next to the second (a page of
/// text).
const LADDER_BYTES: [f64; 6] = [1.26, 1.0, 0.85, 0.52, 0.31, 0.26];
/// The same for sharp screens (phones): a smaller picture is blown up
/// there (2.6× on a typical phone) and looks soft, while JPEG's blocks
/// shrink with the pixels, so quality goes down before size does. A size
/// between full and half saves little on a slow link and costs the server
/// a shrink of every frame, so it goes straight to half.
const SHARP_LADDER: [(f64, u8); 6] = [
    (1.0, 80),
    (1.0, 62),
    (1.0, 48),
    (1.0, 38),
    (0.5, 50),
    (0.5, 36),
];
const SHARP_LADDER_BYTES: [f64; 6] = [1.31, 1.0, 0.87, 0.77, 0.38, 0.31];
/// Screens with this many device pixels per CSS pixel (or more) are sharp.
const SHARP_DPR: f64 = 1.5;
const START_LEVEL: usize = 1;
/// Between movements (typing, a click, a blinking caret): full size, at
/// this quality for each step of `LADDER`.
const IDLE_QUALITY: [u8; 6] = [80, 75, 70, 65, 55, 45];
/// Frames a second a moving page should get before frames get smaller.
const MOVING_FPS: f64 = 20.0;
/// How long a page must stay unchanged after moving (or after a lone
/// change) before the sharp still picture is taken.
const SETTLE: Duration = Duration::from_millis(300);
const SETTLE_LONE: Duration = Duration::from_millis(1200);
/// The still picture: WebP quality, and at most this many pixels.
const STILL_QUALITY: u8 = 85;
const STILL_JPEG_QUALITY: u8 = 80;
const STILL_PIXELS: f64 = 4.6e6;
const STILL_SIDE: f64 = 3200.0;
/// The page's pixel ratio for still pictures (and what pages see as
/// `devicePixelRatio`) at most.
const MAX_DPR: f64 = 3.0;

/// The steps frames take while moving on this device, and their sizes.
fn ladder(view: &Viewport) -> (&'static [(f64, u8); 6], &'static [f64; 6]) {
    if view.dpr >= SHARP_DPR {
        (&SHARP_LADDER, &SHARP_LADDER_BYTES)
    } else {
        (&LADDER, &LADDER_BYTES)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewport {
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
    pub mobile: bool,
}

impl Viewport {
    pub(crate) fn clamped(width: u32, height: u32, dpr: f64, mobile: bool) -> Self {
        let dpr = if dpr.is_finite() {
            dpr.clamp(0.5, 4.0)
        } else {
            1.0
        };
        Self {
            width: width.clamp(MIN_W, MAX_W),
            height: height.clamp(MIN_H, MAX_H),
            dpr,
            mobile,
        }
    }

    /// The device pixel ratio frames are made at: the device's, reduced so
    /// a frame stays within the size caps.
    pub(crate) fn frame_dpr(&self, scale: f64) -> f64 {
        let (w, h) = (self.width as f64, self.height as f64);
        let side = MAX_SIDE / w.max(h);
        let area = (MAX_PIXELS / (w * h)).sqrt();
        self.dpr.min(scale).min(side).min(area).max(0.25)
    }

    /// The pixel ratio the page itself is shown at (what it sees as
    /// `devicePixelRatio`, and still pictures): the device's, within the
    /// still picture caps, never below the frames'.
    pub(crate) fn page_dpr(&self, scale: f64) -> f64 {
        let (w, h) = (self.width as f64, self.height as f64);
        let side = STILL_SIDE / w.max(h);
        let area = (STILL_PIXELS / (w * h)).sqrt();
        self.dpr
            .min(MAX_DPR)
            .min(side)
            .min(area)
            .max(self.frame_dpr(scale))
            .max(1.0)
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            dpr: 1.0,
            mobile: false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Link {
    pub cdp: Arc<Cdp>,
    pub target: String,
    pub session: String,
    /// The browser's run it belongs to (`Live::generation`).
    pub generation: u64,
}

tokio::task_local! {
    /// The run of a machine's browser an agent's action was allowed on
    /// (`Browsers::before_act_as`): while that action goes on, its tabs'
    /// pages are only used on that run, never on one started since (a Stop
    /// there closed the old one; the user's own tile may bring a new one).
    pub(crate) static ALLOWED_RUN: u64;
}

tokio::task_local! {
    /// Set while an agent's browser tool call runs: on a machine's browser
    /// it makes no room (closes no page) unless allowed there on that run.
    pub(crate) static AGENT_CALL: ();
}

/// `f` as an agent's tool call (see [`AGENT_CALL`]).
pub(crate) async fn as_agent<F: std::future::Future>(f: F) -> F::Output {
    AGENT_CALL.scope((), f).await
}

/// May a page of run `current` of a machine's browser be closed to make
/// room now? Always for the user's own requests; for an agent's call, only
/// within its action allowed on that very run.
pub(crate) fn may_make_room(current: Option<u64>) -> bool {
    AGENT_CALL.try_with(|_| ()).is_err()
        || ALLOWED_RUN
            .try_with(|g| *g)
            .is_ok_and(|g| current == Some(g))
}

/// `f` as an agent's action allowed on run `admitted` (`None`: this
/// server's browser, nothing to hold to).
pub(crate) async fn on_run<F: std::future::Future>(admitted: Option<u64>, f: F) -> F::Output {
    match admitted {
        Some(g) => ALLOWED_RUN.scope(g, f).await,
        None => f.await,
    }
}

pub(crate) struct Ident {
    pub key: String,
    pub board: Option<String>,
    /// The session whose agent opened this (headless) tab.
    pub owner: Option<String>,
    /// The chat notices about this tab go to.
    pub chat: Option<String>,
    /// A window a page opened, shown on top of its opener.
    pub popup: bool,
}

/// What a device said it can do when it attached.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Takes {
    /// Several frames at a time (`BrowserAttach::pipelined`).
    pub pipelined: bool,
    /// WebP pictures (it gets the sharp still pictures).
    pub webp: bool,
}

/// A frame on its way to a device.
struct Sent {
    frame: u64,
    at: Instant,
    bytes: usize,
    /// A still picture (big, and once: its round trip says little).
    still: bool,
    /// Nothing else was on its way: its round trip is the connection's own.
    alone: bool,
}

struct ViewerState {
    takes: Takes,
    /// Frames on their way, oldest first.
    in_flight: VecDeque<Sent>,
    last_sent: u64,
    /// Bytes it may have on their way (`pipelined` devices; one frame is
    /// always allowed).
    window: usize,
    /// When the window last changed.
    window_at: Instant,
    /// A frame waited because the window was full (since it last grew).
    limited: bool,
    /// The quickest send → ack time lately (ms), and when it was seen.
    rtt_min: f64,
    rtt_min_at: Instant,
    /// Measuring it afresh: one frame at a time for a round trip.
    probing: bool,
    retry: Option<Instant>,
    /// How fast frames get through (bytes a second, the best lately; 0:
    /// not known yet): (when, rate) of acknowledgements that came while
    /// more frames were on their way, so the link was busy in between.
    rates: VecDeque<(Instant, f64)>,
    /// When the last acknowledgement came, and whether frames were still
    /// on their way after it.
    last_ack: Option<(Instant, bool)>,
}

impl ViewerState {
    fn new(takes: Takes) -> Self {
        Self {
            takes,
            in_flight: VecDeque::new(),
            last_sent: 0,
            window: START_WINDOW,
            window_at: Instant::now(),
            limited: false,
            rtt_min: f64::INFINITY,
            rtt_min_at: Instant::now(),
            probing: true,
            retry: None,
            rates: VecDeque::new(),
            last_ack: None,
        }
    }

    /// How fast frames get through lately (bytes a second; 0: not known).
    fn rate(&self) -> f64 {
        self.rates.iter().map(|r| r.1).fold(0.0, f64::max)
    }

    /// How long a frame of `bytes` takes to get through the link (ms).
    fn send_ms(&self, bytes: usize) -> f64 {
        match self.rate() {
            r if r > 0.0 => bytes as f64 / r * 1000.0,
            _ => 0.0,
        }
    }

    /// Forget frames that were never acknowledged.
    fn expire(&mut self, now: Instant) {
        while self
            .in_flight
            .front()
            .is_some_and(|s| now.duration_since(s.at) > ACK_TIMEOUT)
        {
            self.in_flight.pop_front();
        }
    }

    /// Whether a frame of `bytes` may go now.
    fn has_room(&self, bytes: usize) -> bool {
        if self.in_flight.is_empty() {
            return true;
        }
        let queued: usize = self.in_flight.iter().map(|s| s.bytes).sum();
        // Two at a time whatever their size: one arriving while the next
        // is sent keeps a slow link busy (and shows how fast it is).
        self.takes.pipelined
            && !self.probing
            && self.in_flight.len() < MAX_IN_FLIGHT
            && (self.in_flight.len() < 2 || queued + bytes <= self.window)
    }

    /// Frame `frame` (and every earlier one) arrived: learn from how long
    /// it took. Returns whether anything was in flight.
    fn acked(&mut self, frame: u64, now: Instant) -> bool {
        let mut newest = None;
        let mut bytes = 0;
        while self.in_flight.front().is_some_and(|s| s.frame <= frame) {
            newest = self.in_flight.pop_front();
            bytes += newest.as_ref().map_or(0, |s| s.bytes);
        }
        let Some(sent) = newest else {
            return false;
        };
        // How fast the link carries frames: what came in since the last
        // acknowledgement, if more were on their way then (acks bunched
        // closer than a few ms say little).
        let prev = self.last_ack.replace((now, !self.in_flight.is_empty()));
        if let Some((at, true)) = prev {
            let secs = now.duration_since(at).as_secs_f64();
            if secs >= 0.008 && !sent.still {
                self.rates.push_back((now, bytes as f64 / secs));
            }
        }
        while self
            .rates
            .front()
            .is_some_and(|r| now.duration_since(r.0) > RATE_SPAN)
            || self.rates.len() > 32
        {
            self.rates.pop_front();
        }
        if !self.takes.pipelined || sent.still {
            return true;
        }
        // The window follows the queueing delay: how much longer than the
        // connection's own round trip this frame took. That is measured
        // with nothing else on the way, at first and every ten seconds.
        // Frames that take a while to get through (big ones on a slow
        // link) queue behind each other for that long anyway: up to about
        // one of them waiting is what keeps the link busy, so it counts
        // as no delay.
        let ms = now.duration_since(sent.at).as_secs_f64() * 1000.0;
        let own = self.send_ms(sent.bytes);
        if (sent.alone && self.probing) || ms < self.rtt_min {
            self.rtt_min = ms;
            self.rtt_min_at = now;
            self.probing = false;
        } else if now.duration_since(self.rtt_min_at) > Duration::from_secs(10) {
            self.probing = true;
        }
        let queued = ms - self.rtt_min;
        let settled =
            now.duration_since(self.window_at).as_secs_f64() * 1000.0 > self.rtt_min.max(30.0);
        if !settled {
            return true;
        }
        if queued > (0.3 * self.rtt_min).max(30.0) + 1.5 * own && self.window > MIN_WINDOW {
            self.window = (self.window * 7 / 10).max(MIN_WINDOW);
        } else if queued < (0.1 * self.rtt_min).max(15.0) + own
            && self.limited
            && self.window < MAX_WINDOW
        {
            self.window = (self.window + sent.bytes.max(16 << 10)).min(MAX_WINDOW);
            self.limited = false;
        } else {
            return true;
        }
        self.window_at = now;
        tracing::debug!(
            "window {} KB (round trip {ms:.0} ms, quickest {:.0} ms, {:.0} KB/s)",
            self.window >> 10,
            self.rtt_min,
            self.rate() / 1024.0
        );
        true
    }
}

struct Frame {
    seq: u64,
    data: String,
    mime: &'static str,
    width: u32,
    height: u32,
    scroll: Option<(f64, f64)>,
    /// Only for devices that don't get still pictures (the browser repeats
    /// the picture when its screencast restarts; the still replaces it).
    no_still: bool,
    /// A still picture (sharp, once the page rests).
    still: bool,
}

impl Frame {
    fn bytes(&self) -> usize {
        self.data.len() * 3 / 4
    }
}

pub(crate) struct TabState {
    link: Option<Link>,
    url: String,
    title: String,
    loading: bool,
    back: bool,
    forward: bool,
    zoom: f64,
    favicon: Option<String>,
    /// The page looks like a site's "prove you're a person" check.
    check: bool,
    view: Viewport,
    /// What the target's emulation is set to: (viewport, zoom, ua mobile).
    applied: Option<(Viewport, f64)>,
    viewers: HashMap<u64, ViewerState>,
    seq: u64,
    /// The newest screencast frame.
    latest: Option<Arc<Frame>>,
    /// The sharp picture of it, once taken.
    still: Option<Arc<Frame>>,
    /// The browser's frames we haven't acknowledged: it sends at most three
    /// ahead, then waits for us.
    unacked: Vec<i64>,
    held_since: Option<Instant>,
    /// Screencast running with (JPEG quality, max width, max height).
    streaming: Option<(u8, u32, u32)>,
    /// When the screencast last (re)started: its first frame repeats the
    /// picture.
    restarted: Option<Instant>,
    motion: Motion,
    pressed: u32,
    dialog: Option<(BrowserDialogKind, String, Option<String>)>,
    focus: String,
    sent: Option<BrowserTab>,
    pub(crate) nav_error: Option<String>,
    /// The page last asked for a new window (a popup is on its way).
    pub(crate) window_opened: Option<Instant>,
    /// Explicit window geometry identifies a sign-in popup rather than a tab.
    window_popup: bool,
    persist: Option<(String, Instant)>,
    pub(crate) downloads: Vec<(String, Instant)>,
    pub(super) file_chooser: Option<(String, i64, bool)>,
    pub(super) file_chooser_frame: String,
    pub(crate) chooser: Option<Instant>,
    pub(crate) shot_scale: f64,
    /// An agent action is running (since).
    pub(crate) agent_since: Option<Instant>,
    last_used: Instant,
    runtime_on: bool,
    crashed_at: Option<Instant>,
    /// Its page went away under the agent (the browser stopped, the page
    /// crashed): what the agent is told on its next step here.
    interrupted: Option<String>,
    /// The run of the browser its page was last opened on, until it is
    /// closed on purpose. (When the browser dies, the page's own event loop
    /// may forget the page before the browser's cleanup looks.)
    linked_gen: Option<u64>,
    /// Frozen while nobody watches or uses it (`[browser] freeze_seconds`).
    frozen: bool,
    freeze_after: Option<Duration>,
    /// A popup is open on top of this page.
    has_popup: bool,
    /// The pixel ratio of the screen its window is on (its render scale,
    /// when tabs have screens of their own; see `render.rs`), and the
    /// window.
    placed: f64,
    placement_stale: bool,
    window: Option<i64>,
    /// The screencast must start again (its window moved).
    stream_stale: bool,
    /// Frames for the connection changed size: the render scale may follow.
    resized: bool,
}

/// Whether the page is moving, how big frames are while it does, and when
/// it is due a still picture.
struct Motion {
    moving: bool,
    /// The step of `LADDER` frames are made at while moving.
    level: usize,
    /// When the last few screencast frames came.
    arrivals: VecDeque<Instant>,
    /// Take a still picture then, unless the page changes before.
    settle_at: Option<Instant>,
    /// The screencast frame the last still picture shows (or will).
    still_for: u64,
    /// A still picture is being taken.
    taking: bool,
    /// The last frame waited for a device.
    last_held: bool,
    /// Since `period`: frames that went out, and time the browser waited
    /// for devices (the connection, not the page, set the pace).
    period: Instant,
    sent: u32,
    held: Duration,
    /// The newest frame counted in `sent`.
    counted: u64,
    /// When the level last changed, how long to wait before trying a
    /// sharper one, and when the last try was.
    level_at: Instant,
    backoff: Duration,
    probed_at: Option<Instant>,
}

impl Motion {
    fn new() -> Self {
        Self {
            moving: false,
            level: START_LEVEL,
            arrivals: VecDeque::new(),
            settle_at: None,
            still_for: 0,
            taking: false,
            last_held: false,
            period: Instant::now(),
            sent: 0,
            held: Duration::ZERO,
            counted: 0,
            level_at: Instant::now(),
            backoff: Duration::from_secs(3),
            probed_at: None,
        }
    }

    fn new_period(&mut self) {
        self.period = Instant::now();
        self.sent = 0;
        self.held = Duration::ZERO;
    }

    /// (share of the full frame size, JPEG quality) to stream at now.
    fn params(&self, view: &Viewport) -> (f64, u8) {
        if self.moving {
            ladder(view).0[self.level]
        } else {
            (1.0, IDLE_QUALITY[self.level])
        }
    }
}

impl TabState {
    fn freezable(&self) -> bool {
        !self.frozen
            && self.freeze_after.is_some()
            && self.viewers.is_empty()
            && self.agent_since.is_none()
            && self.link.is_some()
            && !self.has_popup
    }
}

pub struct Tab {
    ident: Mutex<Ident>,
    pub(super) st: Mutex<TabState>,
    /// Serialises making the page target.
    open: tokio::sync::Mutex<()>,
    /// A failed initial open may discard the handle, but not a retained page.
    opened: std::sync::atomic::AtomicBool,
    view_sync: tokio::sync::Mutex<()>,
    /// Serialises agent actions on this tab.
    pub(crate) act: tokio::sync::Mutex<()>,
    /// Bumped on every state change (waiters watch it).
    changed: watch::Sender<u64>,
    /// Wakes the event loop when its next timer moved earlier.
    wake_loop: tokio::sync::Notify,
    shared: Weak<Shared>,
    /// The popup open on top of this page, and the page under this popup.
    child: Mutex<Option<Arc<Tab>>>,
    parent: Mutex<Weak<Tab>>,
}

/// Own the create response (including late replies), exact target and page
/// admission until publication. Cancellation transfers rollback, not the gate.
struct PageCreation {
    tab: Arc<Tab>,
    live: Arc<Live>,
    pending: Option<super::cdp::OwnedReply>,
    target: Option<String>,
    session: Option<String>,
    admission: Option<tokio::sync::OwnedMutexGuard<()>>,
    uncertain: bool,
}
impl PageCreation {
    fn new(
        tab: Arc<Tab>,
        live: Arc<Live>,
        admission: tokio::sync::OwnedMutexGuard<()>,
        params: Value,
    ) -> Self {
        let cdp = live.cdp.clone();
        let pending = cdp.enqueue_owned("Target.createTarget", params);
        Self {
            tab,
            live,
            pending: Some(pending),
            target: None,
            session: None,
            admission: Some(admission),
            uncertain: false,
        }
    }

    async fn target(&mut self) -> Result<String, String> {
        let reply = tokio::time::timeout(
            Duration::from_secs(30),
            self.pending.as_mut().unwrap().wait(),
        )
        .await
        .map_err(|_| "the browser didn't answer Target.createTarget".to_string())?;
        self.pending = None;
        let value = match reply {
            Ok(value) => value,
            Err((code, message)) => {
                self.uncertain = code.is_none();
                return Err(message);
            }
        };
        let target = value["targetId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        self.uncertain = target.is_none();
        self.target = target.clone();
        target.ok_or_else(|| "the browser didn't identify the new target".into())
    }

    fn publish(&mut self) {
        self.admission = None;
    }
}
impl Drop for PageCreation {
    fn drop(&mut self) {
        let Some(admission) = self.admission.take() else {
            return;
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut pending = self.pending.take();
        let target = self.target.take();
        let session = self.session.take();
        let tab = self.tab.clone();
        let live = self.live.clone();
        let cdp = live.cdp.clone();
        let browser = tab.browser();
        let uncertain = self.uncertain;
        // Clear only this creation's partially published link before a
        // failed initial handle is removed. Never unlink a different page.
        if let Some(target) = &target {
            tab.with_state(|st| {
                if st
                    .link
                    .as_ref()
                    .is_some_and(|l| l.target == *target && Arc::ptr_eq(&l.cdp, &cdp))
                {
                    st.link = None;
                    st.linked_gen = None;
                    st.streaming = None;
                    st.loading = false;
                }
            });
        }
        tokio::spawn(async move {
            let _admission = admission;
            if browser.as_ref().is_some_and(|b| b.creation_gone(&live)) {
                return;
            }
            let rollback = async {
                let target = match target {
                    Some(target) => target,
                    None => match pending.as_mut() {
                        Some(reply) => match reply.wait().await {
                            Ok(value) => {
                                match value["targetId"].as_str().filter(|id| !id.is_empty()) {
                                    Some(target) => target.to_string(),
                                    None => {
                                        cdp.wait_closed().await;
                                        return;
                                    }
                                }
                            }
                            Err((Some(_), _)) => return,
                            Err((None, _)) => {
                                cdp.wait_closed().await;
                                return;
                            }
                        },
                        None => {
                            if uncertain {
                                cdp.wait_closed().await;
                            }
                            return;
                        }
                    },
                };
                if let Some(shared) = tab.shared.upgrade() {
                    let same = shared
                        .live
                        .read()
                        .as_ref()
                        .is_some_and(|l| Arc::ptr_eq(&l.cdp, &cdp));
                    if same && let Some((session, _)) = shared.attached.lock().remove(&target) {
                        cdp.unsubscribe(&session);
                    }
                }
                if let Some(session) = session {
                    cdp.unsubscribe(&session);
                }
                let closed = cdp
                    .call(None, "Target.closeTarget", json!({"targetId":target}))
                    .await
                    .is_ok_and(|value| value["success"] == true);
                if !closed {
                    let absent = cdp
                        .call(None, "Target.getTargets", json!({}))
                        .await
                        .is_ok_and(|value| {
                            value["targetInfos"].as_array().is_some_and(|targets| {
                                !targets.iter().any(|info| info["targetId"] == target)
                            })
                        });
                    if !absent {
                        cdp.wait_closed().await;
                    }
                }
            };
            if tokio::time::timeout_at(deadline, rollback).await.is_err() {
                if let Some(browser) = &browser {
                    browser.recover_creation(&live).await;
                } else {
                    cdp.wait_closed().await;
                }
            }
            // EOF alone is not confirmed process/tree teardown. The same
            // existing exit cleanup token gates both profile reuse and this
            // canceled page's admission, including a detached reaper.
            if cdp.is_closed() && browser.as_ref().is_some_and(|b| !b.creation_gone(&live)) {
                live.stopped.cancelled().await;
            }
        });
    }
}
struct InitialOpen(Arc<Tab>);
impl Drop for InitialOpen {
    fn drop(&mut self) {
        let tab = &self.0;
        if tab.is_popup() {
            return;
        }
        let Ok(_open) = tab.open.try_lock() else {
            return;
        };
        if !tab.opened.load(std::sync::atomic::Ordering::Acquire)
            && tab.registered()
            && let Some(owner) = tab.owner()
            && let Some(browser) = tab.browser()
        {
            browser.close_own_tab(&owner, &tab.key());
        }
    }
}

impl Tab {
    pub(crate) fn new(
        shared: &Arc<Shared>,
        key: String,
        board: Option<String>,
        owner: Option<String>,
        chat: Option<String>,
        url: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            ident: Mutex::new(Ident {
                key,
                board,
                owner,
                chat,
                popup: false,
            }),
            st: Mutex::new(TabState {
                link: None,
                url,
                title: String::new(),
                loading: false,
                back: false,
                forward: false,
                zoom: 1.0,
                favicon: None,
                check: false,
                view: Viewport::default(),
                applied: None,
                viewers: HashMap::new(),
                seq: 0,
                latest: None,
                still: None,
                unacked: Vec::new(),
                held_since: None,
                streaming: None,
                restarted: None,
                motion: Motion::new(),
                pressed: 0,
                dialog: None,
                focus: String::new(),
                sent: None,
                nav_error: None,
                window_opened: None,
                window_popup: false,
                persist: None,
                downloads: Vec::new(),
                file_chooser: None,
                file_chooser_frame: String::new(),
                chooser: None,
                shot_scale: 1.0,
                agent_since: None,
                last_used: Instant::now(),
                runtime_on: false,
                crashed_at: None,
                interrupted: None,
                linked_gen: None,
                frozen: false,
                freeze_after: None,
                has_popup: false,
                placed: 1.0,
                placement_stale: false,
                window: None,
                stream_stale: false,
                resized: false,
            }),
            child: Mutex::new(None),
            parent: Mutex::new(Weak::new()),
            open: tokio::sync::Mutex::new(()),
            opened: std::sync::atomic::AtomicBool::new(false),
            view_sync: tokio::sync::Mutex::new(()),
            act: tokio::sync::Mutex::new(()),
            changed: watch::channel(0).0,
            wake_loop: tokio::sync::Notify::new(),
            shared: Arc::downgrade(shared),
        })
    }

    pub(super) fn browser(&self) -> Option<Browser> {
        self.shared.upgrade().map(Browser)
    }
    /// The browser this tab is in.
    pub fn host_browser(&self) -> Option<Browser> {
        self.browser()
    }
    /// The machine whose browser this tab is in (none: this server).
    pub fn machine(&self) -> Option<String> {
        self.browser().and_then(|b| b.machine())
    }

    pub(crate) fn key(&self) -> String {
        self.ident.lock().key.clone()
    }
    pub(crate) fn board(&self) -> Option<String> {
        self.ident.lock().board.clone()
    }
    pub(crate) fn owner(&self) -> Option<String> {
        self.ident.lock().owner.clone()
    }
    pub(crate) fn chat(&self) -> Option<String> {
        self.ident.lock().chat.clone()
    }
    /// Make this (headless) tab the tab of a board.
    pub(crate) fn rekey(&self, board: &str, chat: &str) {
        let mut id = self.ident.lock();
        id.key = board.to_string();
        id.board = Some(board.to_string());
        id.owner = None;
        id.chat = Some(chat.to_string());
    }
    pub(crate) fn url(&self) -> String {
        self.st.lock().url.clone()
    }
    pub(crate) fn title(&self) -> String {
        self.st.lock().title.clone()
    }
    pub(crate) fn set_url(&self, url: &str) {
        self.st.lock().url = url.to_string();
    }
    pub(crate) fn is_live(&self) -> bool {
        self.st
            .lock()
            .link
            .as_ref()
            .is_some_and(|l| !l.cdp.is_closed())
    }
    pub(crate) fn target(&self) -> Option<String> {
        self.st.lock().link.as_ref().map(|l| l.target.clone())
    }
    /// It (or a popup on it) had a page on run `generation` of its browser
    /// that nobody closed on purpose.
    pub(crate) fn had_page_on(self: &Arc<Self>, generation: u64) -> bool {
        self.chain()
            .iter()
            .any(|t| t.st.lock().linked_gen == Some(generation))
    }
    /// Its page (or a popup's on it) went away under it: `note` is what the
    /// agent is told on its next step here.
    pub(crate) fn interrupted(&self, note: &str) {
        self.st.lock().interrupted = Some(note.to_string());
    }
    /// What happened to its page since the agent's last step, once.
    pub(crate) fn take_interrupted(&self) -> Option<String> {
        self.st.lock().interrupted.take()
    }
    /// The devices attached, with the tab's viewport.
    pub(crate) fn viewers(&self) -> Vec<(u64, Viewport, Takes)> {
        let st = self.st.lock();
        st.viewers
            .iter()
            .map(|(id, v)| (*id, st.view, v.takes))
            .collect()
    }
    /// A device shows this tab (or a popup open on it).
    pub(crate) fn watched(self: &Arc<Self>) -> bool {
        self.chain().iter().any(|t| t.viewer_count() > 0)
    }
    pub(crate) fn viewer_count(&self) -> usize {
        self.st.lock().viewers.len()
    }
    pub(crate) fn last_used(&self) -> Instant {
        self.st.lock().last_used
    }
    /// A base page's entire popup chain is one shedding candidate.
    pub(crate) fn memory_use(self: &Arc<Self>) -> (bool, Instant) {
        let mut protected = false;
        let mut used = self.last_used();
        for tab in self.chain() {
            let st = tab.st.lock();
            protected |=
                !st.viewers.is_empty() || st.agent_since.is_some() || tab.open.try_lock().is_err();
            used = used.max(st.last_used);
        }
        (protected, used)
    }

    /// Revalidate the whole chain under nonblocking action/tree/state locks.
    /// A viewer, agent or fresh popup arriving after selection cancels shedding.
    pub(crate) fn shed(self: &Arc<Self>, idle: Duration, note: &str) -> bool {
        Self::shed_all(&[super::PageVictim { tab: self.clone() }], idle, note)
    }

    /// Revalidate all root pages and chains before mutating any. Nonblocking
    /// open guards also exclude requests that are waiting for admission.
    pub(super) fn shed_all(victims: &[super::PageVictim], idle: Duration, note: &str) -> bool {
        let chains: Vec<_> = victims.iter().map(|v| v.tab.chain()).collect();
        let chain: Vec<_> = chains.iter().flatten().collect();
        let Some(_opens) = chain
            .iter()
            .map(|t| t.open.try_lock().ok())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let Some(actions) = chain
            .iter()
            .map(|t| t.act.try_lock().ok())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let Some(mut children) = chain
            .iter()
            .map(|t| t.child.try_lock())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let mut offset = 0;
        for tabs in &chains {
            for (i, child) in children[offset..offset + tabs.len()].iter().enumerate() {
                if child.as_ref().map(Arc::as_ptr) != tabs.get(i + 1).map(Arc::as_ptr) {
                    return false;
                }
            }
            offset += tabs.len();
        }
        let Some(mut states) = chain
            .iter()
            .map(|t| t.st.try_lock())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        if states
            .iter()
            .any(|st| !st.viewers.is_empty() || st.agent_since.is_some())
        {
            return false;
        }
        let mut offset = 0;
        for tabs in &chains {
            let group = &states[offset..offset + tabs.len()];
            if group
                .iter()
                .map(|st| st.last_used)
                .max()
                .is_none_or(|t| t.elapsed() < idle)
                || group
                    .first()
                    .is_none_or(|st| st.link.as_ref().is_none_or(|l| l.cdp.is_closed()))
            {
                return false;
            }
            offset += tabs.len();
        }
        let allowed = ALLOWED_RUN.try_with(|g| *g).ok();
        if states
            .iter()
            .any(|st| allowed.is_some_and(|g| st.link.as_ref().is_some_and(|l| l.generation != g)))
        {
            return false;
        }
        let links: Vec<_> = states
            .iter_mut()
            .filter_map(|st| {
                st.interrupted = Some(note.into());
                st.streaming = None;
                st.unacked.clear();
                st.applied = None;
                st.loading = false;
                st.file_chooser = None;
                st.linked_gen = None;
                st.has_popup = false;
                st.link.take()
            })
            .collect();
        for child in &mut children {
            **child = None;
        }
        drop(states);
        drop(children);
        drop(actions);
        for l in links {
            l.cdp.unsubscribe(&l.session);
            if !l.cdp.is_closed() {
                l.cdp
                    .send(None, "Target.closeTarget", json!({"targetId": l.target}));
            }
        }
        for victim in victims {
            victim.tab.publish();
        }
        true
    }
    pub(crate) fn view(&self) -> (Viewport, f64) {
        let st = self.st.lock();
        (st.view, st.zoom)
    }
    pub(crate) fn loading(&self) -> bool {
        self.st.lock().loading
    }
    pub(crate) fn dialog(&self) -> Option<(BrowserDialogKind, String, Option<String>)> {
        self.st.lock().dialog.clone()
    }
    pub(crate) fn with_state<R>(&self, f: impl FnOnce(&mut TabState) -> R) -> R {
        f(&mut self.st.lock())
    }
    /// The sharpest frames may be (`[browser] scale`).
    pub(crate) fn scale(&self) -> f64 {
        self.shared
            .upgrade()
            .map(|s| f64::from_bits(s.cap.load(std::sync::atomic::Ordering::Relaxed)))
            .unwrap_or(2.0)
    }
    /// The scale the page is drawn at: its window's screen's, or the one
    /// every tab has.
    fn render(&self, st: &TabState) -> f64 {
        match self.browser() {
            Some(b) if b.screens_on() => b.window_scale(st.window, st.placed),
            Some(b) => b.global_scale(),
            None => 1.0,
        }
    }
    pub(crate) fn is_popup(&self) -> bool {
        self.ident.lock().popup
    }
    /// The page on top: this one, or the popup open over it (and so on).
    pub(crate) fn top(self: &Arc<Self>) -> Arc<Tab> {
        let mut t = self.clone();
        for _ in 0..16 {
            let next = t.child.lock().clone();
            match next {
                Some(c) => t = c,
                None => break,
            }
        }
        t
    }
    /// This page and the popups stacked on it.
    pub(crate) fn chain(self: &Arc<Self>) -> Vec<Arc<Tab>> {
        let mut out = vec![self.clone()];
        for _ in 0..16 {
            let next = out.last().and_then(|t| t.child.lock().clone());
            match next {
                Some(c) => out.push(c),
                None => break,
            }
        }
        out
    }
    /// The page under this popup.
    pub(crate) fn opener(&self) -> Option<Arc<Tab>> {
        self.parent.lock().upgrade()
    }
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    pub(crate) fn link(&self) -> Result<Link, String> {
        let link = self
            .st
            .lock()
            .link
            .clone()
            .filter(|l| !l.cdp.is_closed())
            .ok_or_else(|| "the tab isn't open".to_string())?;
        // An agent's action goes on only on the run it was allowed on.
        if ALLOWED_RUN
            .try_with(|g| *g)
            .is_ok_and(|g| g != link.generation)
        {
            return Err(super::RESTARTED.into());
        }
        Ok(link)
    }

    /// Use a chooser from this exact page, never a path supplied to CDP.
    pub(crate) async fn choose_files(
        &self,
        chooser: &str,
        files: &[tessera_proto::Attachment],
    ) -> Result<(), String> {
        let link = self.link()?;
        let (id, node, many) = self
            .st
            .lock()
            .file_chooser
            .clone()
            .ok_or("that file chooser closed")?;
        if id != chooser {
            return Err("that file chooser closed; choose again on the current page".into());
        }
        if !many && files.len() > 1 {
            return Err("this page accepts one file".into());
        }
        let browser = self.browser().ok_or("the browser closed")?;
        let params = if browser.machine().is_some() {
            // The node stages only these bytes inside its browser's own folder.
            json!({"backendNodeId": node, "files": files})
        } else {
            let paths = browser.0.uploads.lock().stage(files)?;
            json!({"backendNodeId": node, "files": paths})
        };
        // Recheck after staging; navigation or a new chooser cancels the old one.
        if self.st.lock().file_chooser.as_ref().map(|c| c.0.as_str()) != Some(chooser) {
            return Err("the page changed while files were chosen; choose again".into());
        }
        self.link()?;
        link.cdp
            .call(
                Some(&link.session),
                if browser.machine().is_some() {
                    "Tessera.chooseFiles"
                } else {
                    "DOM.setFileInputFiles"
                },
                params,
            )
            .await?;
        {
            let mut st = self.st.lock();
            if st.file_chooser.as_ref().map(|c| c.0.as_str()) == Some(chooser) {
                st.file_chooser = None;
            }
        }
        self.publish();
        Ok(())
    }
    /// The tab as clients see it.
    pub(crate) fn state(&self) -> BrowserTab {
        let key = self.key();
        let tab_ids = self.browser().map(|b| b.group(&key)).unwrap_or_default();
        let popup = self.is_popup();
        let opener = self.opener().map(|p| p.url()).filter(|_| popup);
        let st = self.st.lock();
        BrowserTab {
            tab_ids,
            popup,
            opener,
            board_id: key,
            url: st.url.clone(),
            title: st.title.clone(),
            loading: st.loading,
            can_go_back: st.back,
            can_go_forward: st.forward,
            zoom: st.zoom,
            favicon: st.favicon.clone(),
            check: st.check,
            file_chooser: st
                .file_chooser
                .as_ref()
                .filter(|_| st.link.is_some())
                .map(|(id, _, _)| id.clone()),
            file_multiple: st.file_chooser.as_ref().is_some_and(|(_, _, many)| *many),
        }
    }

    /// Tell every client about a change of a board tab's state.
    pub(super) fn publish(&self) {
        self.bump();
        // Clients see the page on top only.
        if self.board().is_none() || self.child.lock().is_some() {
            return;
        }
        let tab = self.state();
        {
            let mut st = self.st.lock();
            if st.sent.as_ref() == Some(&tab) {
                return;
            }
            st.sent = Some(tab.clone());
        }
        if let Some(s) = self.shared.upgrade() {
            let _ = s.events().send(Event::BrowserState { tab });
        }
    }

    pub(crate) fn notice(&self, level: NoticeLevel, message: &str) {
        if let Some(s) = self.shared.upgrade() {
            let _ = s.events().send(Event::Notice {
                session_id: self.chat().or_else(|| self.owner()),
                level,
                message: message.to_string(),
            });
        }
    }

    // ---- the page target ---------------------------------------------------

    /// The live page target, made (and loaded with the tab's address) if
    /// the browser restarted or the tab was closed.
    pub(crate) async fn ensure_live(self: &Arc<Self>) -> Result<Link, String> {
        self.ensure_live_on(None).await
    }

    /// A retired or removed root cannot make an unregistered Chrome page.
    /// Chrome-created popups keep their existing opener-chain lifecycle.
    pub(super) fn registered(self: &Arc<Self>) -> bool {
        if self.is_popup() {
            return true;
        }
        self.shared.upgrade().is_some_and(|s| {
            s.tabs
                .lock()
                .get(&self.key())
                .is_some_and(|t| Arc::ptr_eq(t, self))
        })
    }

    /// The same for an agent's action allowed on run `admitted` of its
    /// machine's browser: only on that run, never a replacement run.
    pub(crate) async fn ensure_live_on(
        self: &Arc<Self>,
        admitted: Option<u64>,
    ) -> Result<Link, String> {
        let _initial = InitialOpen(self.clone());
        let result = self.ensure_live_inner(admitted).await;
        if result.is_ok() {
            self.opened
                .store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }

    async fn ensure_live_inner(self: &Arc<Self>, admitted: Option<u64>) -> Result<Link, String> {
        if !self.registered() {
            return Err("This tab is closed; open a new tab.".into());
        }
        let same =
            || admitted.is_none_or(|g| self.browser().and_then(|b| b.generation()) == Some(g));
        if !same() {
            return Err(super::RESTARTED.into());
        }
        if let Ok(l) = self.link() {
            return Ok(l);
        }
        let _g = self.open.lock().await;
        if !same() {
            return Err(super::RESTARTED.into());
        }
        if let Ok(l) = self.link() {
            return Ok(l);
        }
        let browser = self.browser().ok_or("the server is shutting down")?;
        let live = browser.live().await?;
        if admitted.is_some_and(|g| live.generation != g) {
            return Err(super::RESTARTED.into());
        }
        let guard = browser.admit_page(Some(self)).await?;
        self.create_page_admitted(live, guard).await
    }

    /// The caller already committed the cap/pressure plan under this guard.
    /// Do not sample pressure or admit again between eviction and creation.
    async fn create_page_admitted(
        self: &Arc<Self>,
        live: Arc<Live>,
        admission: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Link, String> {
        let browser = self.browser().ok_or("the server is shutting down")?;
        if !self.registered() {
            return Err("This tab is closed; open a new tab.".into());
        }
        let cdp = live.cdp.clone();
        let mut params = json!({"url": "about:blank", "background": true, "newWindow": true});
        if live.headless {
            params["left"] = json!(64);
            params["top"] = json!(64);
            params["width"] = json!(1280);
            params["height"] = json!(800);
        }
        let mut creation = PageCreation::new(self.clone(), live.clone(), admission, params);
        let target = creation.target().await?;
        // Every new page is attached by the browser and waits for us.
        let session = browser.claim_attached(&target).await?;
        creation.session = Some(session.clone());
        let link = Link {
            cdp: cdp.clone(),
            target,
            session,
            generation: live.generation,
        };
        {
            let mut st = self.st.lock();
            st.placed = 1.0;
            st.placement_stale = false;
            st.window = None;
        }
        self.setup(&link, false).await?;
        let s = Some(link.session.as_str());
        let url = self.url();
        if !url.is_empty() && url != "about:blank" {
            self.with_state(|st| st.loading = true);
            let _ = cdp.call(s, "Page.navigate", json!({"url": url})).await;
        }
        if !self.registered() {
            return Err("This tab is closed; open a new tab.".into());
        }
        self.opened
            .store(true, std::sync::atomic::Ordering::Release);
        self.publish();
        creation.publish();
        Ok(link)
    }

    /// Independent script and network holds cover debugger resume. Noopener
    /// targets can bootstrap an inert document, reloaded only after protections.
    async fn setup(self: &Arc<Self>, link: &Link, bootstrap: bool) -> Result<(), String> {
        let cdp = &link.cdp;
        let mut rx = cdp.subscribe(&link.session);
        let s = Some(link.session.as_str());
        let host = self.shared.upgrade().and_then(|s| s.host());
        let patterns = host.as_ref().and_then(|h| h.own_port()).map(|port| {
            let patterns: Vec<Value> = [
                format!("*://127.0.0.1:{port}/*"),
                format!("*://localhost:{port}/*"),
                format!("*://[::1]:{port}/*"),
                format!("*://0.0.0.0:{port}/*"),
                format!("*.localhost:{port}/*"),
            ]
            .into_iter()
            .map(|p| json!({"urlPattern": p}))
            .collect();
            patterns
        });
        let (reload, held_events) = cdp
            .initialize_page(
                &link.session,
                NO_KEYS_JS,
                patterns,
                bootstrap.then_some(link.target.as_str()),
                &mut rx,
            )
            .await?;
        let freeze_after = host
            .map(|h| h.browser_config().freeze_seconds)
            .filter(|s| *s > 0.0 && s.is_finite())
            .map(|s| Duration::from_secs_f64(s.max(1.0)));
        {
            let mut st = self.st.lock();
            st.link = Some(link.clone());
            st.linked_gen = Some(link.generation);
            st.applied = None;
            st.streaming = None;
            st.unacked.clear();
            st.held_since = None;
            st.runtime_on = false;
            st.frozen = false;
            st.freeze_after = freeze_after;
            st.latest = None;
            st.dialog = None;
            st.focus.clear();
            st.has_popup = false;
            st.last_used = Instant::now();
        }
        for event in held_events {
            self.on_event(link, event);
        }
        tokio::spawn(self.clone().run(link.clone(), rx));
        cdp.call(
            s,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": FOCUS_JS, "worldName": WORLD}),
        )
        .await?;
        for name in ["__tsrFocus", "__tsrTitle", "__tsrCheck"] {
            cdp.call(
                s,
                "Runtime.addBinding",
                json!({"name": name, "executionContextName": WORLD}),
            )
            .await?;
        }
        cdp.call(
            s,
            "Page.setInterceptFileChooserDialog",
            json!({"enabled": true}),
        )
        .await?;
        cdp.call(
            s,
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled": true}),
        )
        .await?;
        self.apply_view(link).await?;
        // A page that already started (a popup) gets the scripts now too.
        let _ = self.eval_in(link, FOCUS_JS, false).await;
        if reload {
            cdp.call(s, "Page.reload", json!({"ignoreCache":true}))
                .await?;
        }
        Ok(())
    }

    /// A page opened a window (`session` waits for us): show it on top of
    /// the page that is on top now. The opener stays as it is, so the popup
    /// can talk to it and close itself, as sign-in windows do.
    pub(crate) async fn open_popup(
        self: &Arc<Self>,
        live: Arc<Live>,
        target: String,
        session: String,
        url: String,
        can_access_opener: bool,
    ) {
        let link = Link {
            cdp: live.cdp.clone(),
            target,
            session,
            generation: live.generation,
        };
        let close = |l: &Link| {
            l.cdp
                .send(None, "Target.closeTarget", json!({"targetId": l.target}))
        };
        let Some(shared) = self.shared.upgrade() else {
            return close(&link);
        };
        let under = self.top();
        // Ordinary target=_blank pages are independent saved tabs. Explicit
        // popup geometry and opener-access windows keep the sign-in stack.
        let ordinary = !can_access_opener && !under.st.lock().window_popup && !under.is_popup();
        if ordinary && let (Some(browser), Some(board)) = (self.browser(), self.board()) {
            let initial = if url.starts_with("http") {
                url.clone()
            } else {
                self.url()
            };
            let _admission = match browser.admit_page(Some(self)).await {
                Ok(guard) => guard,
                Err(_) => {
                    close(&link);
                    return;
                }
            };
            match browser.save_page(&board, &initial) {
                Ok(board) => {
                    if let Ok(tab) = browser.board_tab(&board.id) {
                        if let Err(e) = tab.setup(&link, true).await {
                            tracing::debug!("couldn't set up a new tab: {e}");
                            close(&link);
                            return;
                        }
                        tab.refresh_title(&link);
                        tab.refresh_history(&link);
                        browser.group_page(&self.key(), &board.id);
                        tab.publish();
                        return;
                    }
                }
                Err(_) => {
                    close(&link);
                    return;
                }
            }
        }
        if self.chain().len() > MAX_POPUPS {
            tracing::debug!("too many popups; closing a new one");
            return close(&link);
        }
        let child = {
            let id = under.ident.lock();
            Tab::new(
                &shared,
                id.key.clone(),
                id.board.clone(),
                id.owner.clone(),
                id.chat.clone(),
                if url.is_empty() {
                    "about:blank".into()
                } else {
                    url
                },
            )
        };
        child.ident.lock().popup = true;
        {
            let (view, zoom) = under.view();
            // Its window opens on the screen of the page that opened it.
            let placed = under.st.lock().placed;
            let mut st = child.st.lock();
            st.view = view;
            st.zoom = zoom;
            st.back = true;
            st.placed = placed;
        }
        // On the stack first, so the browser's news about it finds it.
        *child.parent.lock() = Arc::downgrade(&under);
        {
            let mut popup = under.child.lock();
            let mut st = under.st.lock();
            if !st
                .link
                .as_ref()
                .is_some_and(|l| l.generation == live.generation)
            {
                close(&link);
                return; // shedding or exit won before this popup was stacked
            }
            *popup = Some(child.clone());
            st.has_popup = true;
            st.last_used = Instant::now();
        }
        if let Err(e) = child.setup(&link, !can_access_opener).await {
            tracing::debug!("couldn't set up a popup: {e}");
            close(&link);
            child.popped();
            return;
        }
        // It may have loaded before its events were on.
        child.refresh_title(&link);
        child.refresh_history(&link);
        // The devices watching the opener now watch the popup.
        let moved = move_viewers(&under, &child);
        if !moved.is_empty() {
            child.sync_view().await;
            under.sync_view().await;
            for id in moved {
                child.greet(id);
            }
        }
        child.publish();
    }

    /// This popup closed: the page under it is on top again, with the
    /// devices that watched the popup.
    fn popped(self: &Arc<Self>) {
        let Some(under) = self.opener() else { return };
        {
            let mut c = under.child.lock();
            if !c.as_ref().is_some_and(|c| Arc::ptr_eq(c, self)) {
                return;
            }
            *c = None;
        }
        let mut viewers = Vec::new();
        for t in self.chain() {
            viewers.extend(t.viewers());
            for (id, ..) in t.viewers() {
                t.st.lock().viewers.remove(&id);
            }
            if !Arc::ptr_eq(&t, self) {
                t.unlink();
            }
        }
        {
            let mut st = self.st.lock();
            st.link = None;
            st.streaming = None;
        }
        {
            let mut st = under.st.lock();
            st.has_popup = false;
            st.sent = None;
        }
        for (id, view, takes) in &viewers {
            under.attach(*id, *view, *takes);
        }
        let under2 = under.clone();
        tokio::spawn(async move {
            under2.wake().await;
            let _ = under2.show().await;
            under2.sync_view().await;
            for (id, ..) in viewers {
                under2.greet(id);
            }
            under2.publish();
            if let Ok(l) = under2.link() {
                under2.refresh_title(&l);
                under2.refresh_history(&l);
            }
        });
        self.bump();
    }

    /// Forget the page target (closing it).
    pub(crate) fn unlink(&self) {
        // An agent's action allowed on another run leaves this one's page
        // (and its popups) alone; checked again where the link is taken.
        let allowed = ALLOWED_RUN.try_with(|g| *g).ok();
        let other_run = |st: &TabState| {
            allowed.is_some_and(|g| st.link.as_ref().is_some_and(|l| l.generation != g))
        };
        if other_run(&self.st.lock()) {
            return;
        }
        let child = self.child.lock().take();
        if let Some(c) = child {
            for t in c.chain() {
                for (id, view, takes) in t.viewers() {
                    t.st.lock().viewers.remove(&id);
                    self.attach(id, view, takes);
                }
                t.unlink();
            }
            self.st.lock().has_popup = false;
        }
        let link = {
            let mut st = self.st.lock();
            if other_run(&st) {
                drop(st);
                self.publish();
                return;
            }
            st.streaming = None;
            st.unacked.clear();
            st.applied = None;
            st.loading = false;
            st.file_chooser = None;
            st.linked_gen = None;
            st.link.take()
        };
        if let Some(l) = link {
            l.cdp.unsubscribe(&l.session);
            if !l.cdp.is_closed() {
                l.cdp
                    .send(None, "Target.closeTarget", json!({"targetId": l.target}));
            }
        }
        self.publish();
    }

    /// Close for good (its board or session is gone).
    pub(crate) fn close(&self) {
        let child = self.child.lock().take();
        if let Some(c) = child {
            c.close();
        }
        let viewers: Vec<u64> = self.st.lock().viewers.keys().copied().collect();
        for v in viewers {
            self.detach(v);
        }
        self.unlink();
    }

    pub(crate) fn on_target_gone(self: &Arc<Self>) {
        if self.is_popup() {
            self.popped();
            return;
        }
        if self.st.lock().link.is_none() {
            return;
        }
        let watched = {
            let mut st = self.st.lock();
            st.link = None;
            st.streaming = None;
            st.unacked.clear();
            st.applied = None;
            !st.viewers.is_empty()
        };
        self.publish();
        if watched {
            // Closed by the page (window.close()): bring it back for the
            // people watching.
            let me = self.clone();
            tokio::spawn(async move {
                if me.ensure_live().await.is_ok() {
                    me.sync_view().await;
                }
            });
        }
    }

    pub(crate) fn on_crashed(self: &Arc<Self>) {
        let again = {
            let mut st = self.st.lock();
            let again = st
                .crashed_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30));
            st.crashed_at = Some(Instant::now());
            st.loading = false;
            again
        };
        self.notice(
            NoticeLevel::Warning,
            if again {
                "The page crashed again; reload it to try once more."
            } else {
                "The page crashed; reloading it."
            },
        );
        // The page of a popup: the agent works through the tab under it.
        let mut base = self.clone();
        for _ in 0..16 {
            match base.opener() {
                Some(o) => base = o,
                None => break,
            }
        }
        base.st.lock().interrupted = Some(if again {
            "The page crashed again in the browser; `reload` to try once more. Anything typed into it and not yet submitted is gone.".into()
        } else {
            "The page crashed in the browser and was reloaded, so anything typed into it and not yet submitted is gone; look at it before going on.".into()
        });
        if !again && let Ok(l) = self.link() {
            l.cdp.send(Some(&l.session), "Page.reload", json!({}));
        }
    }

    pub(crate) fn on_info(&self, title: &str, url: &str) {
        {
            let mut st = self.st.lock();
            if st.title == title && (url.is_empty() || st.url == url) {
                return;
            }
            st.title = crate::util::one_line(title, 200);
            if !url.is_empty() && url != "about:blank" {
                st.url = url.to_string();
            }
        }
        self.publish();
    }

    // ---- viewers and frames ------------------------------------------------

    pub(crate) fn attach(&self, viewer: u64, view: Viewport, takes: Takes) {
        let mut st = self.st.lock();
        st.view = view;
        st.last_used = Instant::now();
        let fresh = !st.viewers.contains_key(&viewer);
        let v = st
            .viewers
            .entry(viewer)
            .or_insert_with(|| ViewerState::new(takes));
        // A new device gets the current picture right away (attaching again
        // after a resize keeps what was learnt about its connection).
        v.last_sent = 0;
        v.in_flight.clear();
        if v.takes != takes {
            *v = ViewerState::new(takes);
        }
        if fresh {
            // Another device: frames sized for its connection (as it was
            // for its last tab, if any).
            st.motion.level = self
                .shared
                .upgrade()
                .and_then(|s| s.hub.levels.lock().get(&viewer).copied())
                .unwrap_or(START_LEVEL);
            st.motion.backoff = Duration::from_secs(3);
            st.motion.new_period();
        }
    }

    pub(crate) fn detach(&self, viewer: u64) {
        let empty = {
            let mut st = self.st.lock();
            if st.viewers.remove(&viewer).is_none() {
                return;
            }
            st.viewers.is_empty()
        };
        if empty {
            let me = self;
            if let Ok(l) = me.link() {
                me.stop_stream(&l);
            }
        } else {
            // Its pending frame no longer holds the others up.
            self.release_hold();
        }
    }

    /// Bring a device that just attached up to date: dialog, focus, frame.
    pub(crate) fn greet(&self, viewer: u64) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let Some(tx) = shared.viewer_tx(viewer) else {
            return;
        };
        let key = self.key();
        let (dialog, focus) = {
            let st = self.st.lock();
            (st.dialog.clone(), st.focus.clone())
        };
        if let Some((kind, message, default)) = dialog {
            let _ = tx.try_send(Event::BrowserDialog {
                board_id: key.clone(),
                kind,
                message,
                default,
            });
        }
        if !focus.is_empty() {
            let _ = tx.try_send(Event::BrowserFocus {
                board_id: key,
                editable: true,
                input_type: focus,
            });
        }
        let mut st = self.st.lock();
        self.deliver(&mut st);
    }

    /// Emulate the latest attacher's device and stream at its size.
    pub(crate) async fn sync_view(self: &Arc<Self>) {
        let _guard = self.view_sync.lock().await;
        let Ok(link) = self.link() else { return };
        if self.browser().and_then(|b| b.running()).is_none() {
            return;
        }
        if let Err(error) = self.apply_view(&link).await {
            tracing::warn!(%error, "web view metrics could not be applied");
            return;
        }
        let (want, runtime) = {
            let st = self.st.lock();
            (!st.viewers.is_empty(), st.runtime_on)
        };
        let s = Some(link.session.as_str());
        // Focus reports (for phone keyboards) only matter while watched.
        if want != runtime {
            let _ = link
                .cdp
                .call(
                    s,
                    if want {
                        "Runtime.enable"
                    } else {
                        "Runtime.disable"
                    },
                    json!({}),
                )
                .await;
            if want {
                // Bindings reach pages that loaded before Runtime was on
                // only when added again.
                for name in ["__tsrFocus", "__tsrTitle", "__tsrCheck"] {
                    let _ = link
                        .cdp
                        .call(
                            s,
                            "Runtime.addBinding",
                            json!({"name": name, "executionContextName": WORLD}),
                        )
                        .await;
                }
            }
            self.st.lock().runtime_on = want;
            // A page that loaded unwatched: whether it wants a check.
            if want {
                let _ = self
                    .eval_in(
                        &link,
                        "globalThis.__tsrLook && globalThis.__tsrLook()",
                        false,
                    )
                    .await;
            }
        }
        if want {
            self.start_stream(&link).await;
        } else {
            self.stop_stream(&link);
        }
    }

    async fn apply_view(&self, link: &Link) -> Result<(), String> {
        // Its window on the screen of its render scale first (a move drops
        // the page's emulation and screencast: both are set again).
        let moved = self.place(link).await?;
        let (view, zoom, applied) = {
            let mut st = self.st.lock();
            if moved {
                st.applied = None;
            }
            if st.applied != Some((st.view, st.zoom)) {
                // Another size: the still picture is out of date.
                st.still = None;
                st.motion.taking = false;
            }
            (st.view, st.zoom, st.applied)
        };
        if applied == Some((view, zoom)) {
            return Ok(());
        }
        let s = Some(link.session.as_str());
        if moved {
            link.cdp
                .call(s, "Emulation.clearDeviceMetricsOverride", json!({}))
                .await?;
        }
        // The device's own pixel ratio: still pictures are taken at it
        // (screencast frames come at the render scale whatever this is).
        let dpr = view.page_dpr(self.scale());
        let w = (view.width as f64 / zoom).round().max(50.0);
        let h = (view.height as f64 / zoom).round().max(50.0);
        link.cdp
            .call(
                s,
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": w as u32,
                    "height": h as u32,
                    "deviceScaleFactor": dpr * zoom,
                    "mobile": view.mobile,
                    "screenWidth": w as u32,
                    "screenHeight": h as u32,
                }),
            )
            .await?;
        let mobile_changed = applied.is_none_or(|(v, _)| v.mobile != view.mobile);
        if mobile_changed {
            let _ = link
                .cdp
                .call(
                    s,
                    "Emulation.setTouchEmulationEnabled",
                    json!({"enabled": view.mobile, "maxTouchPoints": 5}),
                )
                .await;
            // A phone's scrollbars show while it scrolls and then fade out:
            // a second of extra frames after every scroll, for a thin line.
            let _ = link
                .cdp
                .call(
                    s,
                    "Emulation.setScrollbarsHidden",
                    json!({"hidden": view.mobile}),
                )
                .await;
        }
        let mut st = self.st.lock();
        st.applied = Some((view, zoom));
        // Metrics can change CSS geometry without changing screencast pixel
        // bounds. Restart anyway: Chrome may still be waiting on old frames,
        // and a still captured before the change must never replace this view.
        // Release Chrome's parked frames, not device acknowledgements. Clearing
        // the device queue below makes old acknowledgements harmless (frame IDs
        // stay monotonic), and a new frame can probe the new geometry promptly.
        self.release(&mut st, 0);
        st.stream_stale = true;
        st.latest = None;
        st.still = None;
        st.motion.taking = false;
        st.motion.arrivals.clear();
        st.motion.settle_at = None;
        for viewer in st.viewers.values_mut() {
            viewer.in_flight.clear();
            viewer.probing = true;
        }
        Ok(())
    }

    /// Put the tab's window on the screen of the scale its frames should be
    /// drawn at (when tabs have screens of their own). Returns whether it
    /// moved.
    async fn place(&self, link: &Link) -> Result<bool, String> {
        let Some(browser) = self.browser().filter(|b| b.screens_on()) else {
            return Ok(false);
        };
        let (view, zoom, from, window, share, stale) = {
            let st = self.st.lock();
            // Only what devices watch is drawn for them.
            if st.viewers.is_empty() {
                return Ok(false);
            }
            (
                st.view,
                st.zoom,
                st.placed,
                st.window,
                ladder(&st.view).0[st.motion.level].0,
                st.placement_stale,
            )
        };
        let cap = self.scale();
        let f = view.frame_dpr(cap);
        let need = (f * zoom).min(cap);
        let css = (view.width as f64 / zoom) * (view.height as f64 / zoom) / 1e6;
        // No sharper than the frames a slow connection takes (the ladder,
        // as `stream_params` sizes them).
        let takes = (f * share).max(f.min(1.0)) * zoom;
        let to = browser
            .render_scale_for(need, css)
            .min(super::render::step_up(takes));
        if !stale && (to - browser.window_scale(window, from)).abs() < 1e-3 {
            return Ok(false);
        }
        let window = match window {
            Some(w) => w,
            None => {
                let w = link
                    .cdp
                    .call(
                        None,
                        "Browser.getWindowForTarget",
                        json!({"targetId": link.target}),
                    )
                    .await?;
                let w = w["windowId"].as_i64().ok_or("the page has no window")?;
                self.st.lock().window = Some(w);
                w
            }
        };
        browser.place_window(&link.cdp, window, from, to).await?;
        tracing::debug!("tab renders at {to}x (was {from}x)");
        let mut st = self.st.lock();
        st.placed = to;
        st.placement_stale = false;
        st.stream_stale = true;
        st.still = None;
        st.motion.taking = false;
        Ok(true)
    }

    /// (JPEG quality, max width, max height) the screencast should run at.
    fn stream_params(&self, st: &TabState) -> (u8, u32, u32) {
        // At most as sharp as the page is drawn (frames never scale up).
        let dpr = st
            .view
            .frame_dpr(self.scale())
            .min(self.render(st) / st.zoom);
        let (share, quality) = st.motion.params(&st.view);
        // Smaller for a slow connection, but not below the page's own
        // size (lower quality from there).
        let dpr = (dpr * share).max(dpr.min(1.0));
        let side = |css: u32| (css as f64 * dpr).round().max(1.0) as u32;
        (quality, side(st.view.width), side(st.view.height))
    }

    async fn start_stream(&self, link: &Link) {
        let (want, current, stale) = {
            let st = self.st.lock();
            (self.stream_params(&st), st.streaming, st.stream_stale)
        };
        if current == Some(want) && !stale {
            return;
        }
        let s = Some(link.session.as_str());
        if current.is_some() {
            let _ = link.cdp.call(s, "Page.stopScreencast", json!({})).await;
        }
        {
            // A new size or a new device: the still picture is out of date.
            let mut st = self.st.lock();
            st.streaming = Some(want);
            st.stream_stale = false;
            st.restarted = Some(Instant::now());
            st.unacked.clear();
            st.still = None;
            st.motion.taking = false;
            end_hold(&mut st);
        }
        if let Err(error) = link
            .cdp
            .call(s, "Page.startScreencast", screencast(want))
            .await
        {
            let mut st = self.st.lock();
            st.streaming = None;
            st.stream_stale = true;
            tracing::warn!(%error, "web view screencast could not start");
        }
    }

    /// Restart the screencast if the motion state wants other frames now.
    fn restream(&self, st: &mut TabState) -> bool {
        // sync_view coalesces placement, metrics and bounds into one restart.
        // Don't race that pending restart from a motion/settle event.
        if st.stream_stale {
            return false;
        }
        let want = self.stream_params(st);
        let (Some(current), Some(l)) = (st.streaming, st.link.clone()) else {
            return false;
        };
        if current == want {
            return false;
        }
        tracing::debug!(
            "screencast {want:?} ({}, level {})",
            if st.motion.moving { "moving" } else { "still" },
            st.motion.level
        );
        st.streaming = Some(want);
        st.restarted = Some(Instant::now());
        // The browser forgets the frames it waited on.
        st.unacked.clear();
        end_hold(st);
        // Frames of another size take another time: measure the round
        // trip afresh.
        for v in st.viewers.values_mut() {
            v.probing = true;
        }
        let s = Some(l.session.as_str());
        l.cdp.send(s, "Page.stopScreencast", json!({}));
        l.cdp.send(s, "Page.startScreencast", screencast(want));
        true
    }

    fn stop_stream(&self, link: &Link) {
        let was = {
            let mut st = self.st.lock();
            st.held_since = None;
            st.unacked.clear();
            st.motion.moving = false;
            st.motion.settle_at = None;
            st.motion.taking = false;
            let was = st.streaming.take().is_some();
            if st.viewers.is_empty() {
                st.latest = None;
                st.still = None;
            }
            was
        };
        if was {
            let s = Some(link.session.as_str());
            link.cdp.send(s, "Page.stopScreencast", json!({}));
            if self.st.lock().runtime_on {
                link.cdp.send(s, "Runtime.disable", json!({}));
                self.st.lock().runtime_on = false;
            }
        }
    }

    /// Send every device the newest picture it hasn't got, as far as its
    /// window allows. Returns whether the newest screencast frame went out.
    fn deliver(&self, st: &mut TabState) -> bool {
        let Some(latest) = st.latest.clone() else {
            return false;
        };
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        let still = st.still.clone();
        let key = self.key();
        let now = Instant::now();
        let mut took = false;
        for (id, v) in st.viewers.iter_mut() {
            let frame = match &still {
                Some(s) if v.takes.webp => s,
                // A repeat its still picture is about to replace.
                _ if latest.no_still && v.takes.webp => continue,
                _ => &latest,
            };
            if v.last_sent >= frame.seq {
                continue;
            }
            v.expire(now);
            if !v.has_room(frame.bytes()) {
                v.limited = true;
                continue;
            }
            let Some(tx) = shared.viewer_tx(*id) else {
                continue;
            };
            let ev = Event::BrowserFrame {
                board_id: key.clone(),
                frame: frame.seq,
                mime: frame.mime.into(),
                data: frame.data.clone(),
                width: frame.width,
                height: frame.height,
                scroll_x: frame.scroll.map(|s| s.0),
                scroll_y: frame.scroll.map(|s| s.1),
            };
            match tx.try_send(ev) {
                Ok(()) => {
                    let alone = v.in_flight.is_empty();
                    v.in_flight.push_back(Sent {
                        frame: frame.seq,
                        at: now,
                        bytes: frame.bytes(),
                        still: frame.still,
                        alone,
                    });
                    v.last_sent = frame.seq;
                    v.retry = None;
                    took |= frame.seq == latest.seq;
                    shared
                        .frames
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .frame_bytes
                        .fetch_add(frame.bytes() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                Err(_) => v.retry = Some(now + Duration::from_millis(250)),
            }
        }
        if took && st.motion.counted < latest.seq {
            st.motion.counted = latest.seq;
            st.motion.sent += 1;
        }
        took
    }

    fn on_frame(&self, link: &Link, mut params: Value) {
        let ack = params["sessionId"].as_i64().unwrap_or(0);
        let data = match params.get_mut("data").map(Value::take) {
            Some(Value::String(d)) => d,
            _ => return,
        };
        let meta = &params["metadata"];
        let width = meta["deviceWidth"].as_f64().unwrap_or(0.0).round() as u32;
        let height = meta["deviceHeight"].as_f64().unwrap_or(0.0).round() as u32;
        // Where the page is scrolled to (not while it is pinch-zoomed).
        let scroll =
            ((meta["pageScaleFactor"].as_f64().unwrap_or(1.0) - 1.0).abs() < 0.01).then(|| {
                (
                    meta["scrollOffsetX"].as_f64().unwrap_or(0.0),
                    meta["scrollOffsetY"].as_f64().unwrap_or(0.0),
                )
            });
        let s = Some(link.session.as_str());
        let mut st = self.st.lock();
        // Chrome can finish encoding the old size after the metrics ACK.
        // Check every frame when geometry is known, allowing DIP rounding.
        // A move or unknown apply must not prevent frames from arriving.
        let other_size = st.applied.is_some_and(|(v, zoom)| {
            let off = |css: u32, got: u32| ((css as f64 / zoom).round() - got as f64).abs() > 1.0;
            off(v.width, width) || off(v.height, height)
        });
        let stilling = st.motion.taking || st.still.is_some();
        let same = stilling && st.latest.as_ref().is_some_and(|f| f.data == data);
        if st.streaming.is_none() || st.stream_stale || st.viewers.is_empty() || other_size || same
        {
            drop(st);
            link.cdp
                .send(s, "Page.screencastFrameAck", json!({"sessionId": ack}));
            return;
        }
        let now = Instant::now();
        // The first frame after a (re)start repeats the picture (however
        // late it comes while a still picture is on its way or shown: a
        // slow server may draw it after the still).
        let stilled = st.motion.taking || st.still.is_some();
        let repeat = st
            .restarted
            .take()
            .is_some_and(|t| stilled || now.duration_since(t) < Duration::from_millis(300));
        // Frame numbers are unique across tabs, so a late ack for the page
        // under a popup can't be taken for one of the popup's frames.
        let seq = self.next_seq(&st);
        st.seq = seq;
        let no_still = repeat && (st.motion.taking || st.still.is_some());
        if no_still && st.motion.taking {
            st.motion.still_for = seq;
        }
        let was_held = st.motion.last_held;
        st.latest = Some(Arc::new(Frame {
            seq,
            data,
            mime: "image/jpeg",
            width,
            height,
            scroll,
            no_still,
            still: false,
        }));
        let restarted = if repeat {
            false
        } else {
            // A change: the still picture is out of date, and the page may
            // be moving.
            st.still = None;
            st.motion.taking = false;
            self.note_change(&mut st, now, was_held)
        };
        let took = self.deliver(&mut st);
        tracing::trace!(
            "frame #{seq}: {} KB{}{}{}, level {}{}, windows {:?}",
            st.latest.as_ref().map_or(0, |f| f.bytes() / 1024),
            if repeat { ", repeat" } else { "" },
            if took { ", sent" } else { ", waits" },
            if restarted { ", restarted" } else { "" },
            st.motion.level,
            if st.motion.moving { " moving" } else { "" },
            st.viewers
                .values()
                .map(|v| (v.in_flight.len(), v.window >> 10))
                .collect::<Vec<_>>()
        );
        st.unacked.push(ack);
        if repeat || restarted {
            self.release(&mut st, 0);
        } else if took {
            // On its way: let the browser paint the next one meanwhile.
            let keep = if self.room(&st) { 0 } else { PARKED };
            self.release(&mut st, keep);
        } else {
            // Every device is busy: the browser waits until one is free.
            st.motion.last_held = true;
            st.held_since.get_or_insert(now);
            st.motion.settle_at = None;
        }
    }

    fn next_seq(&self, st: &TabState) -> u64 {
        self.shared
            .upgrade()
            .map(|s| {
                s.frame_ids
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1
            })
            .unwrap_or(st.seq + 1)
    }

    /// The page changed: is it moving, and are frames the right size for
    /// the connection? Returns whether the screencast restarted.
    fn note_change(&self, st: &mut TabState, now: Instant, was_held: bool) -> bool {
        let m = &mut st.motion;
        m.arrivals.push_back(now);
        while m.arrivals.len() > 4 {
            m.arrivals.pop_front();
        }
        let within = |ms: u64| {
            m.arrivals
                .iter()
                .filter(|t| now.duration_since(**t) < Duration::from_millis(ms))
                .count()
        };
        // Three changes in a moment (or two while devices are behind):
        // scrolling, an animation, a video.
        let moving = within(600) >= 3 || (was_held && within(1000) >= 2);
        if moving && !m.moving {
            m.moving = true;
            m.new_period();
            return self.restream(st);
        }
        if m.moving && self.pace(st, now) {
            return self.restream(st);
        }
        false
    }

    /// While moving, every second and a half (sooner when far behind):
    /// frames smaller if the connection (or the device) can't take enough
    /// of them, sharper again when it has room to spare. Returns whether
    /// the level changed.
    fn pace(&self, st: &mut TabState, now: Instant) -> bool {
        let held_now = st
            .held_since
            .map_or(Duration::ZERO, |t| now.duration_since(t));
        // Frames of this size a second the quickest device's link carries
        // (0: not measured yet). Early on fewer get through than that,
        // while its window opens.
        let size = st.latest.as_ref().map_or(0, |f| f.bytes()).max(1) as f64;
        let carries = st.viewers.values().map(|v| v.rate()).fold(0.0, f64::max) / size;
        let (steps, sizes) = ladder(&st.view);
        let m = &mut st.motion;
        let secs = now.duration_since(m.period).as_secs_f64();
        let held = (m.held + held_now).as_secs_f64() / secs.max(0.001);
        let fps = m.sent as f64 / secs.max(0.001);
        // Far too slow (a new connection on a slow link): no need to wait
        // for the whole period to see it.
        let hopeless =
            secs >= 0.5 && held > 0.6 && fps < MOVING_FPS / 2.0 && carries < MOVING_FPS / 2.0;
        if secs < 1.5 && !hopeless {
            return false;
        }
        let level = m.level;
        if held > 0.25 && fps < MOVING_FPS && level + 1 < steps.len() {
            // As much smaller as frames a second fall short.
            let want = sizes[level] * fps.max(carries.min(MOVING_FPS)) / MOVING_FPS;
            m.level = (level + 1..steps.len())
                .find(|l| sizes[*l] <= want)
                .unwrap_or(steps.len() - 1);
            // A sharper level that didn't hold: wait longer before the next try.
            if m.probed_at
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
            {
                m.backoff = (m.backoff * 2).min(Duration::from_secs(30));
            }
            m.probed_at = None;
        } else if level > 0
            && (held < 0.1 || fps > 2.0 * MOVING_FPS)
            && now.duration_since(m.level_at) >= m.backoff
        {
            m.level -= 1;
            m.probed_at = Some(now);
        } else if now.duration_since(m.level_at) > Duration::from_secs(15) {
            m.backoff = Duration::from_secs(3);
        }
        m.new_period();
        if m.level == level {
            return false;
        }
        tracing::debug!("level {level} → {} ({fps:.1} fps, held {held:.2})", m.level);
        m.level_at = now;
        st.resized = true;
        if let Some(shared) = self.shared.upgrade() {
            let mut levels = shared.hub.levels.lock();
            for id in st.viewers.keys() {
                levels.insert(*id, st.motion.level);
            }
        }
        true
    }

    /// Nothing changed for a moment: frames go back to full size, and
    /// devices that draw WebP get a sharp picture. Returns what to capture.
    fn rest(&self, st: &mut TabState) -> Option<Link> {
        let latest = st.latest.as_ref()?.seq;
        let was_moving = std::mem::take(&mut st.motion.moving);
        st.motion.arrivals.clear();
        let wanted = st.viewers.values().any(|v| v.takes.webp)
            && st.still.is_none()
            && !st.motion.taking
            && st.motion.still_for < latest;
        if wanted {
            st.motion.taking = true;
            st.motion.still_for = latest;
        }
        if was_moving {
            // Its first frame repeats the picture: devices without still
            // pictures get it in full quality.
            self.restream(st);
        }
        if !wanted {
            return None;
        }
        st.link.clone()
    }

    /// Take the still picture of the newest frame: WebP, or on a slow
    /// server JPEG (several times quicker to make, twice the bytes).
    async fn take_still(&self, link: Link) {
        let (still_for, applied) = {
            let st = self.st.lock();
            (st.motion.still_for, st.applied)
        };
        let slow = self.browser().is_some_and(|b| b.slow());
        let (format, quality, mime) = if slow {
            ("jpeg", STILL_JPEG_QUALITY, "image/jpeg")
        } else {
            ("webp", STILL_QUALITY, "image/webp")
        };
        let t0 = Instant::now();
        let r = link
            .cdp
            .call(
                Some(&link.session),
                "Page.captureScreenshot",
                json!({"format": format, "quality": quality}),
            )
            .await;
        let took = t0.elapsed().as_millis();
        let data = r
            .ok()
            .and_then(|v| v["data"].as_str().map(str::to_string))
            .filter(|d| !d.is_empty());
        let mut st = self.st.lock();
        // A capture started before metrics changed cannot consume a newer
        // capture's taking flag or label its pixels with the new geometry.
        if st.motion.still_for != still_for || st.applied != applied {
            return;
        }
        let current = std::mem::take(&mut st.motion.taking)
            && st
                .latest
                .as_ref()
                .is_some_and(|l| l.seq == st.motion.still_for);
        let (Some(data), true) = (data, current) else {
            return;
        };
        let latest = st.latest.clone().unwrap();
        let seq = self.next_seq(&st);
        tracing::debug!(
            "still picture #{seq} ({} KB, {took} ms)",
            data.len() * 3 / 4 / 1024
        );
        st.still = Some(Arc::new(Frame {
            seq,
            data,
            mime,
            width: latest.width,
            height: latest.height,
            scroll: latest.scroll,
            no_still: false,
            still: true,
        }));
        self.deliver(&mut st);
    }

    /// A device showed frame `frame` (and the ones before it).
    pub(crate) fn ack(&self, viewer: u64, frame: u64) {
        let mut st = self.st.lock();
        let now = Instant::now();
        st.last_used = now;
        let Some(v) = st.viewers.get_mut(&viewer) else {
            return;
        };
        if !v.acked(frame, now) {
            return;
        }
        let up_to_date = st.latest.as_ref().is_none_or(|f| f.seq <= frame);
        let took = self.deliver(&mut st);
        if (took || up_to_date) && !st.unacked.is_empty() {
            let keep = if self.room(&st) { 0 } else { PARKED };
            self.release(&mut st, keep);
        } else {
            // A send that failed sets a retry time for the event loop.
            self.wake_loop.notify_one();
        }
    }

    /// A device could take another frame now.
    fn room(&self, st: &TabState) -> bool {
        let next = st.latest.as_ref().map_or(0, |f| f.bytes());
        st.viewers.values().any(|v| v.has_room(next))
    }

    /// Let the browser paint on: acknowledge the frames it waits on, all
    /// but the newest `keep` (it paints one more frame for each one
    /// acknowledged, so with every device busy it paints just the next).
    fn release(&self, st: &mut TabState, keep: usize) {
        let n = st.unacked.len().saturating_sub(keep);
        let link = st.link.clone();
        for ack in st.unacked.drain(..n) {
            if let Some(l) = &link {
                l.cdp.send(
                    Some(&l.session),
                    "Page.screencastFrameAck",
                    json!({"sessionId": ack}),
                );
            }
        }
        if st.unacked.is_empty() {
            st.motion.last_held = false;
            end_hold(st);
        } else {
            st.held_since.get_or_insert_with(Instant::now);
            st.motion.settle_at = None;
        }
        // The event loop may sleep past its new timers (when to look for a
        // still moment, or for devices that went quiet).
        self.wake_loop.notify_one();
    }

    /// Stop waiting on devices that went quiet.
    fn release_hold(&self) {
        let mut st = self.st.lock();
        let now = Instant::now();
        for v in st.viewers.values_mut() {
            v.expire(now);
        }
        let took = self.deliver(&mut st);
        let free = self.room(&st) || st.viewers.is_empty();
        if (took || free) && !st.unacked.is_empty() {
            // The held frame went out (or someone is free): paint on.
            self.release(&mut st, 0);
        } else if !st.unacked.is_empty() {
            // Still waiting on every device: look again later, not now.
            if let Some(t) = st.held_since.replace(now) {
                st.motion.held += now.duration_since(t);
            }
        }
    }

    fn send_viewers(&self, ev: impl Fn(&str) -> Event) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let key = self.key();
        let ids: Vec<u64> = self.st.lock().viewers.keys().copied().collect();
        for id in ids {
            if let Some(tx) = shared.viewer_tx(id) {
                let _ = tx.try_send(ev(&key));
            }
        }
    }

    // ---- the event loop ----------------------------------------------------

    async fn run(self: Arc<Self>, link: Link, mut rx: mpsc::UnboundedReceiver<CdpEvent>) {
        loop {
            let deadline = {
                let st = self.st.lock();
                let mut d: Option<Instant> = st.held_since.map(|t| t + ACK_TIMEOUT);
                if st.unacked.is_empty()
                    && let Some(t) = st.motion.settle_at
                {
                    d = Some(d.map_or(t, |d: Instant| d.min(t)));
                }
                let newest = st
                    .still
                    .as_ref()
                    .or(st.latest.as_ref())
                    .map_or(0, |f| f.seq);
                for v in st.viewers.values() {
                    if let Some(r) = v.retry {
                        d = Some(d.map_or(r, |d: Instant| d.min(r)));
                    }
                    // A device that is behind waits on a frame that may be lost.
                    if v.last_sent < newest
                        && let Some(s) = v.in_flight.front()
                    {
                        let t = s.at + ACK_TIMEOUT + Duration::from_millis(1);
                        d = Some(d.map_or(t, |d: Instant| d.min(t)));
                    }
                }
                if let Some((_, t)) = &st.persist {
                    let p = *t + Duration::from_millis(1500);
                    d = Some(d.map_or(p, |d: Instant| d.min(p)));
                }
                if st.freezable()
                    && let Some(after) = st.freeze_after
                {
                    let f = st.last_used + after;
                    d = Some(d.map_or(f, |d: Instant| d.min(f)));
                }
                d
            };
            let ev = match deadline {
                Some(d) => tokio::select! {
                    ev = rx.recv() => ev,
                    _ = tokio::time::sleep_until(d.into()) => {
                        self.on_timer();
                        continue;
                    }
                    _ = self.wake_loop.notified() => continue,
                },
                None => tokio::select! {
                    ev = rx.recv() => ev,
                    _ = self.wake_loop.notified() => continue,
                },
            };
            let Some(ev) = ev else { break };
            self.on_event(&link, ev);
        }
        // The session ended (target closed or the browser stopped).
        let ours = {
            let mut st = self.st.lock();
            let ours = st.link.as_ref().is_some_and(|l| l.session == link.session);
            if ours {
                st.link = None;
                st.streaming = None;
                st.unacked.clear();
                st.applied = None;
                st.loading = false;
            }
            ours
        };
        if ours {
            self.publish();
        }
    }

    fn on_timer(self: &Arc<Self>) {
        let now = Instant::now();
        let persist = {
            let mut st = self.st.lock();
            for v in st.viewers.values_mut() {
                if v.retry.is_some_and(|r| r <= now) {
                    v.retry = None;
                }
            }
            match &st.persist {
                Some((_, t)) if now.duration_since(*t) >= Duration::from_millis(1500) => {
                    st.persist.take().map(|(u, _)| u)
                }
                _ => None,
            }
        };
        if let Some(url) = persist
            && let (Some(board), Some(host)) =
                (self.board(), self.shared.upgrade().and_then(|s| s.host()))
        {
            host.board_navigated(&board, &url);
        }
        // Unwatched and unused for a while: freeze the page (no scripts,
        // timers or painting) until someone looks or acts again.
        let freeze = {
            let mut st = self.st.lock();
            let due = st.freezable()
                && st
                    .freeze_after
                    .is_some_and(|after| now >= st.last_used + after);
            if due {
                st.frozen = true;
            }
            due.then(|| st.link.clone()).flatten()
        };
        if let Some(l) = freeze {
            l.cdp.send(
                Some(&l.session),
                "Page.setWebLifecycleState",
                json!({"state": "frozen"}),
            );
        }
        self.release_hold();
        // The page stopped changing.
        let still = {
            let mut st = self.st.lock();
            let due = st.unacked.is_empty() && st.motion.settle_at.is_some_and(|t| now >= t);
            if due {
                st.motion.settle_at = None;
            }
            if due && !st.viewers.is_empty() {
                self.rest(&mut st)
            } else {
                None
            }
        };
        if let Some(link) = still {
            let me = self.clone();
            tokio::spawn(async move { me.take_still(link).await });
        }
    }

    /// Someone is about to use the tab: note it, and thaw a frozen page.
    pub(crate) async fn wake(&self) {
        let link = {
            let mut st = self.st.lock();
            // An agent's action allowed on another run doesn't touch this one.
            let allowed = ALLOWED_RUN.try_with(|g| *g).ok();
            if allowed.is_some_and(|g| st.link.as_ref().is_some_and(|l| l.generation != g)) {
                return;
            }
            st.last_used = Instant::now();
            if !std::mem::take(&mut st.frozen) {
                return;
            }
            st.link.clone()
        };
        if let Some(l) = link {
            let _ = l
                .cdp
                .call(
                    Some(&l.session),
                    "Page.setWebLifecycleState",
                    json!({"state": "active"}),
                )
                .await;
        }
    }

    fn on_event(self: &Arc<Self>, link: &Link, ev: CdpEvent) {
        let p = &ev.params;
        let popup = self.is_popup();
        let main = |frame: &str| frame == link.target;
        match ev.method.as_str() {
            "Page.screencastFrame" => {
                self.on_frame(link, ev.params);
                // Smaller (or larger) frames for the connection: drawn at a
                // scale to match.
                if std::mem::take(&mut self.st.lock().resized) {
                    let me = self.clone();
                    tokio::spawn(async move { me.sync_view().await });
                }
            }
            "Page.frameNavigated" => {
                let f = &p["frame"];
                if f.get("parentId").is_some_and(|v| !v.is_null()) {
                    let cleared = {
                        let mut st = self.st.lock();
                        if f["id"].as_str() == Some(st.file_chooser_frame.as_str()) {
                            st.file_chooser.take().is_some()
                        } else {
                            false
                        }
                    };
                    if cleared {
                        self.publish();
                    }
                    return;
                }
                let error = f["unreachableUrl"].as_str().map(str::to_string);
                let url = error
                    .clone()
                    .or_else(|| {
                        f["url"]
                            .as_str()
                            .map(|u| format!("{u}{}", f["urlFragment"].as_str().unwrap_or("")))
                    })
                    .unwrap_or_default();
                let blur = {
                    let mut st = self.st.lock();
                    st.nav_error = error.map(|u| format!("couldn't load {u}"));
                    st.url = url.clone();
                    st.favicon = None;
                    st.check = false;
                    st.file_chooser = None;
                    if url.starts_with("http") && !popup {
                        st.persist = Some((url, Instant::now()));
                    }
                    !std::mem::take(&mut st.focus).is_empty()
                };
                if blur {
                    self.send_viewers(|k| Event::BrowserFocus {
                        board_id: k.into(),
                        editable: false,
                        input_type: String::new(),
                    });
                }
                self.refresh_history(link);
                self.publish();
            }
            "Page.frameDetached" => {
                let cleared = {
                    let mut st = self.st.lock();
                    if p["frameId"].as_str() == Some(st.file_chooser_frame.as_str()) {
                        st.file_chooser.take().is_some()
                    } else {
                        false
                    }
                };
                if cleared {
                    self.publish();
                }
            }
            "Page.navigatedWithinDocument" => {
                if !main(p["frameId"].as_str().unwrap_or_default()) {
                    return;
                }
                let url = p["url"].as_str().unwrap_or_default().to_string();
                {
                    let mut st = self.st.lock();
                    st.url = url.clone();
                    if url.starts_with("http") && !popup {
                        st.persist = Some((url, Instant::now()));
                    }
                }
                self.refresh_history(link);
                self.publish();
            }
            "Page.frameStartedLoading" => {
                if main(p["frameId"].as_str().unwrap_or_default()) {
                    self.st.lock().loading = true;
                    self.publish();
                }
            }
            "Page.frameStoppedLoading" => {
                if main(p["frameId"].as_str().unwrap_or_default()) {
                    self.st.lock().loading = false;
                    self.refresh_history(link);
                    self.publish();
                }
            }
            "Page.domContentEventFired" => self.refresh_title(link),
            "Page.loadEventFired" => {
                self.st.lock().loading = false;
                self.publish();
                self.refresh_title(link);
                let me = self.clone();
                let link = link.clone();
                tokio::spawn(async move { me.fetch_favicon(&link).await });
            }
            "Page.windowOpen" => {
                let mut st = self.st.lock();
                st.window_opened = Some(Instant::now());
                st.window_popup = p["windowFeatures"].as_array().is_some_and(|f| {
                    f.iter().any(|v| {
                        v.as_str().is_some_and(|s| {
                            s.starts_with("width=") || s.starts_with("height=") || s == "popup"
                        })
                    })
                });
            }
            "Page.javascriptDialogOpening" => {
                let kind = match p["type"].as_str().unwrap_or("alert") {
                    "confirm" => BrowserDialogKind::Confirm,
                    "prompt" => BrowserDialogKind::Prompt,
                    "beforeunload" => BrowserDialogKind::Beforeunload,
                    _ => BrowserDialogKind::Alert,
                };
                let message =
                    crate::util::truncate(p["message"].as_str().unwrap_or_default(), 2000);
                let default = p["defaultPrompt"]
                    .as_str()
                    .filter(|d| !d.is_empty())
                    .map(str::to_string);
                let watched = {
                    let mut st = self.st.lock();
                    st.dialog = Some((kind, message.clone(), default.clone()));
                    !st.viewers.is_empty()
                };
                self.bump();
                if watched {
                    self.send_viewers(|k| Event::BrowserDialog {
                        board_id: k.into(),
                        kind,
                        message: message.clone(),
                        default: default.clone(),
                    });
                } else if matches!(
                    kind,
                    BrowserDialogKind::Alert | BrowserDialogKind::Beforeunload
                ) && self.board().is_some()
                    && self.with_state(|st| st.agent_since.is_none())
                {
                    // Nobody is looking at this tile: don't leave the page
                    // stuck on an alert or a "leave this page?" question.
                    // (An agent acting here answers itself.)
                    let me = self.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if me.viewer_count() == 0 && me.dialog().is_some_and(|d| d.0 == kind) {
                            let _ = me.answer_dialog(true, None).await;
                        }
                    });
                }
            }
            "Page.javascriptDialogClosed" => {
                let had = self.st.lock().dialog.take().is_some();
                self.bump();
                if had {
                    self.send_viewers(|k| Event::BrowserDialogClosed { board_id: k.into() });
                }
            }
            "Page.fileChooserOpened" => {
                let Some(node) = p["backendNodeId"].as_i64().filter(|n| *n > 0) else {
                    return;
                };
                let mut st = self.st.lock();
                st.chooser = Some(Instant::now());
                st.file_chooser_frame = p["frameId"].as_str().unwrap_or_default().to_string();
                st.file_chooser = Some((
                    crate::util::random_token(16),
                    node,
                    p["mode"] == "selectMultiple",
                ));
                drop(st);
                self.publish();
            }
            "Runtime.bindingCalled" if p["name"] == "__tsrTitle" => {
                let title = p["payload"].as_str().unwrap_or_default().to_string();
                self.on_info(&title, "");
            }
            "Runtime.bindingCalled" if p["name"] == "__tsrCheck" => {
                let check = p["payload"].as_str().is_some_and(|v| !v.is_empty());
                let changed = std::mem::replace(&mut self.st.lock().check, check) != check;
                if changed {
                    self.publish();
                }
            }
            "Runtime.bindingCalled" => {
                if p["name"] != "__tsrFocus" {
                    return;
                }
                let kind = crate::util::truncate(p["payload"].as_str().unwrap_or_default(), 40);
                let changed = {
                    let mut st = self.st.lock();
                    let changed = st.focus != kind;
                    st.focus = kind.clone();
                    changed
                };
                if changed {
                    self.send_viewers(|k| Event::BrowserFocus {
                        board_id: k.into(),
                        editable: !kind.is_empty(),
                        input_type: kind.clone(),
                    });
                }
            }
            "Fetch.requestPaused" => {
                // Initial all-network hold can leave an ordinary navigation
                // paused. Only after setup has succeeded may those continue;
                // the protected harness port is always denied.
                let own_port = self
                    .shared
                    .upgrade()
                    .and_then(|s| s.host())
                    .and_then(|h| h.own_port());
                let blocked = own_port.is_none_or(|port| {
                    url::Url::parse(p["request"]["url"].as_str().unwrap_or_default())
                        .ok()
                        .is_none_or(|u| u.port_or_known_default() == Some(port))
                });
                link.cdp.send(
                    Some(&link.session),
                    if blocked {
                        "Fetch.failRequest"
                    } else {
                        "Fetch.continueRequest"
                    },
                    if blocked {
                        json!({"requestId":p["requestId"],"errorReason":"BlockedByClient"})
                    } else {
                        json!({"requestId":p["requestId"]})
                    },
                );
            }
            "Inspector.targetCrashed" => self.on_crashed(),
            _ => {}
        }
    }

    /// Ask for the page's title (title changes aren't reported by events).
    fn refresh_title(self: &Arc<Self>, link: &Link) {
        let link = link.clone();
        let me = self.clone();
        tokio::spawn(async move {
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
                me.on_info(
                    info["title"].as_str().unwrap_or_default(),
                    info["url"].as_str().unwrap_or_default(),
                );
            }
        });
    }

    fn refresh_history(self: &Arc<Self>, link: &Link) {
        let link = link.clone();
        let me = self.clone();
        tokio::spawn(async move {
            let Ok(h) = link
                .cdp
                .call(Some(&link.session), "Page.getNavigationHistory", json!({}))
                .await
            else {
                return;
            };
            let i = h["currentIndex"].as_i64().unwrap_or(0);
            let n = h["entries"].as_array().map_or(0, |e| e.len()) as i64;
            // The first entry is the blank page every tab starts on.
            let first_real = h["entries"]
                .as_array()
                .and_then(|e| e.first())
                .is_some_and(|e| e["url"] != "about:blank");
            {
                let popup = me.is_popup();
                let mut st = me.st.lock();
                st.back = popup || i > if first_real { 0 } else { 1 };
                st.forward = i + 1 < n;
            }
            me.publish();
        });
    }

    /// The site's icon as a small `data:` URL (shown in the tile's bar).
    async fn fetch_favicon(&self, link: &Link) {
        if self.board().is_none() || self.viewer_count() == 0 {
            return;
        }
        let href = self
            .eval_in(
                link,
                "(() => { const l = document.querySelector('link[rel~=\"icon\" i]'); return l ? l.href : (location.protocol.startsWith('http') ? location.origin + '/favicon.ico' : ''); })()",
                false,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|h| h.starts_with("http") || h.starts_with("data:image/"));
        let Some(href) = href else { return };
        let icon = if href.starts_with("data:image/") {
            (href.len() < 48 << 10).then_some(href)
        } else {
            self.load_resource(link, &href, 48 << 10)
                .await
                .and_then(|bytes| {
                    let mime = sniff_image(&bytes)?;
                    use base64::Engine as _;
                    Some(format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&bytes)
                    ))
                })
        };
        if icon.is_some() {
            self.st.lock().favicon = icon;
            self.publish();
        }
    }

    /// Load a small resource as the page would (no cookies sent).
    async fn load_resource(&self, link: &Link, url: &str, max: usize) -> Option<Vec<u8>> {
        let s = Some(link.session.as_str());
        let r = link
            .cdp
            .call(
                s,
                "Network.loadNetworkResource",
                json!({"frameId": link.target, "url": url, "options": {"disableCache": false, "includeCredentials": false}}),
            )
            .await
            .ok()?;
        let res = &r["resource"];
        if res["success"] != true {
            return None;
        }
        let stream = res["stream"].as_str()?.to_string();
        let mut out = Vec::new();
        loop {
            let chunk = link
                .cdp
                .call(None, "IO.read", json!({"handle": stream, "size": 16384}))
                .await
                .ok()?;
            let data = chunk["data"].as_str().unwrap_or_default();
            if chunk["base64Encoded"] == true {
                use base64::Engine as _;
                out.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .ok()?,
                );
            } else {
                out.extend(data.as_bytes());
            }
            if chunk["eof"] == true || out.len() > max {
                break;
            }
        }
        link.cdp.send(None, "IO.close", json!({"handle": stream}));
        (out.len() <= max && !out.is_empty()).then_some(out)
    }

    pub(crate) fn download_started(&self, guid: &str) {
        let mut st = self.st.lock();
        st.downloads
            .retain(|(_, t)| t.elapsed() < Duration::from_secs(600));
        st.downloads.push((guid.to_string(), Instant::now()));
    }

    // ---- what devices send ---------------------------------------------------

    /// Saved native tabs can share a Chrome window. Reattaching the active
    /// tile must select its page, otherwise Chrome stops painting the old tab.
    pub(crate) async fn show(&self) -> Result<(), String> {
        if self
            .browser()
            .is_none_or(|b| b.group(&self.key()).is_empty())
        {
            return Ok(());
        }
        // A native _blank page shares its opener's window. Another member
        // may have moved it since our cached metrics were applied.
        {
            let mut st = self.st.lock();
            st.placement_stale = true;
            st.applied = None;
        }
        let link = self.link()?;
        link.cdp
            .call(Some(&link.session), "Page.bringToFront", json!({}))
            .await?;
        Ok(())
    }
    /// Relay a device's input. Events go out in order without waiting for
    /// the page (a click that opens a dialog blocks its own answer).
    pub(crate) async fn input(&self, event: BrowserInputEvent) -> Result<(), String> {
        let link = self.link()?;
        let s = Some(link.session.as_str());
        let (vw, vh) = {
            let mut st = self.st.lock();
            st.last_used = Instant::now();
            let (v, z) = (st.view, st.zoom);
            (v.width as f64 / z, v.height as f64 / z)
        };
        let fx = |x: f64| if x.is_finite() { x.clamp(0.0, vw) } else { 0.0 };
        let fy = |y: f64| if y.is_finite() { y.clamp(0.0, vh) } else { 0.0 };
        match event {
            BrowserInputEvent::Mouse {
                kind,
                x,
                y,
                button,
                clicks,
                dx,
                dy,
                modifiers,
            } => {
                let (name, bit) = match button {
                    BrowserMouseButton::None => ("none", 0),
                    BrowserMouseButton::Left => ("left", 1),
                    BrowserMouseButton::Right => ("right", 2),
                    BrowserMouseButton::Middle => ("middle", 4),
                    BrowserMouseButton::Back => ("back", 8),
                    BrowserMouseButton::Forward => ("forward", 16),
                };
                let pressed = {
                    let mut st = self.st.lock();
                    match kind {
                        BrowserMouseKind::Down => st.pressed |= bit,
                        BrowserMouseKind::Up => st.pressed &= !bit,
                        _ => {}
                    }
                    st.pressed
                };
                let ty = match kind {
                    BrowserMouseKind::Down => "mousePressed",
                    BrowserMouseKind::Up => "mouseReleased",
                    BrowserMouseKind::Move => "mouseMoved",
                    BrowserMouseKind::Wheel => "mouseWheel",
                };
                let mut ev = json!({
                    "type": ty,
                    "x": fx(x),
                    "y": fy(y),
                    "modifiers": modifiers & 0xf,
                    "button": name,
                    "buttons": pressed,
                    "clickCount": clicks.clamp(if matches!(kind, BrowserMouseKind::Down | BrowserMouseKind::Up) { 1 } else { 0 }, 3),
                    "pointerType": "mouse",
                });
                if kind == BrowserMouseKind::Wheel {
                    ev["deltaX"] = json!(if dx.is_finite() {
                        dx.clamp(-10_000.0, 10_000.0)
                    } else {
                        0.0
                    });
                    ev["deltaY"] = json!(if dy.is_finite() {
                        dy.clamp(-10_000.0, 10_000.0)
                    } else {
                        0.0
                    });
                }
                link.cdp.send(s, "Input.dispatchMouseEvent", ev);
            }
            BrowserInputEvent::Touch {
                kind,
                points,
                modifiers,
            } => {
                let pts: Vec<Value> = points
                    .iter()
                    .take(10)
                    .map(|p| json!({"x": fx(p.x), "y": fy(p.y), "id": p.id}))
                    .collect();
                let ty = match (kind, pts.is_empty()) {
                    (BrowserTouchKind::Start, false) => "touchStart",
                    (BrowserTouchKind::Move, false) => "touchMove",
                    // Some fingers lifted, others still down.
                    (BrowserTouchKind::End, false) => "touchMove",
                    (BrowserTouchKind::Cancel, _) => "touchCancel",
                    (_, true) => "touchEnd",
                };
                let pts = if matches!(ty, "touchEnd" | "touchCancel") {
                    Vec::new()
                } else {
                    pts
                };
                link.cdp.send(
                    s,
                    "Input.dispatchTouchEvent",
                    json!({"type": ty, "touchPoints": pts, "modifiers": modifiers & 0xf}),
                );
            }
            BrowserInputEvent::Key {
                kind,
                key,
                code,
                text,
                modifiers,
            } => {
                if key.len() > 40 || code.len() > 40 {
                    return Err("unknown key".into());
                }
                let down = kind == BrowserKeyKind::Down;
                let text = match text {
                    Some(t) if t.chars().count() <= 4 => Some(t),
                    Some(_) => {
                        return Err(
                            "a key types at most a few characters; send text instead".into()
                        );
                    }
                    None if down => keys::implied_text(&key, modifiers),
                    None => None,
                };
                link.cdp.send(
                    s,
                    "Input.dispatchKeyEvent",
                    keys::event(down, &key, &code, text.as_deref(), modifiers),
                );
            }
            BrowserInputEvent::Text { text } => {
                if text.len() > 200_000 {
                    return Err("that's too much text to insert at once".into());
                }
                if !text.is_empty() {
                    link.cdp.send(s, "Input.insertText", json!({"text": text}));
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn nav(self: &Arc<Self>, action: BrowserNavAction) -> Result<(), String> {
        self.st.lock().last_used = Instant::now();
        match action {
            BrowserNavAction::Go { url } => self.go(&super::normalize_address(&url)?).await,
            BrowserNavAction::Back => self.history(-1).await,
            BrowserNavAction::Forward => self.history(1).await,
            BrowserNavAction::Reload => {
                let l = self.link()?;
                l.cdp
                    .call(Some(&l.session), "Page.reload", json!({}))
                    .await
                    .map(|_| ())
            }
            BrowserNavAction::Stop => {
                let l = self.link()?;
                l.cdp
                    .call(Some(&l.session), "Page.stopLoading", json!({}))
                    .await
                    .map(|_| ())
            }
            BrowserNavAction::Zoom { factor } => {
                if !factor.is_finite() {
                    return Err("zoom must be a number".into());
                }
                self.st.lock().zoom = (factor * 100.0).round().clamp(25.0, 500.0) / 100.0;
                self.sync_view().await;
                self.publish();
                Ok(())
            }
        }
    }

    /// Open an address in this tab.
    pub(crate) async fn go(&self, url: &str) -> Result<(), String> {
        let own = self
            .shared
            .upgrade()
            .and_then(|s| s.host())
            .and_then(|h| h.own_port());
        if super::is_own_server(url, own) {
            return Err("Tessera's own server can't be opened in the web view".into());
        }
        let l = self.link()?;
        {
            let mut st = self.st.lock();
            st.url = url.to_string();
            st.loading = true;
            st.nav_error = None;
        }
        self.publish();
        let r = l
            .cdp
            .call(Some(&l.session), "Page.navigate", json!({"url": url}))
            .await?;
        if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            let mut st = self.st.lock();
            st.loading = false;
            st.nav_error = Some(e.to_string());
            drop(st);
            self.publish();
            // Chrome shows its own error page; the tab stays usable.
            return Err(format!("couldn't open {url}: {e}"));
        }
        Ok(())
    }

    async fn history(&self, step: i64) -> Result<(), String> {
        let l = self.link()?;
        let s = Some(l.session.as_str());
        let h = l
            .cdp
            .call(s, "Page.getNavigationHistory", json!({}))
            .await?;
        let i = h["currentIndex"].as_i64().unwrap_or(0) + step;
        let entries = h["entries"].as_array().cloned().unwrap_or_default();
        let first = step < 0
            && (i < 0 || (i == 0 && entries.first().is_some_and(|e| e["url"] == "about:blank")));
        if first && self.is_popup() {
            // Nothing to go back to in a popup: close it (the page under it
            // comes back).
            l.cdp
                .send(None, "Target.closeTarget", json!({"targetId": l.target}));
            let mut rx = self.changes();
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                while self.st.lock().link.is_some() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            return Ok(());
        }
        let Some(e) = usize::try_from(i).ok().and_then(|i| entries.get(i)) else {
            return Err(if step < 0 {
                "there is no previous page".into()
            } else {
                "there is no next page".into()
            });
        };
        if e["url"] == "about:blank" && step < 0 {
            return Err("there is no previous page".into());
        }
        l.cdp
            .call(
                s,
                "Page.navigateToHistoryEntry",
                json!({"entryId": e["id"]}),
            )
            .await
            .map(|_| ())
    }

    pub(crate) async fn answer_dialog(
        &self,
        accept: bool,
        text: Option<String>,
    ) -> Result<(), String> {
        if self.st.lock().dialog.is_none() {
            return Err("no dialog is open".into());
        }
        let l = self.link()?;
        let mut p = json!({"accept": accept});
        if let Some(t) = text {
            p["promptText"] = json!(crate::util::truncate(&t, 10_000));
        }
        let r = l
            .cdp
            .call(Some(&l.session), "Page.handleJavaScriptDialog", p)
            .await;
        if self.st.lock().dialog.take().is_some() {
            self.send_viewers(|k| Event::BrowserDialogClosed { board_id: k.into() });
        }
        self.bump();
        r.map(|_| ())
    }
}

/// `Page.startScreencast` parameters for (JPEG quality, max width, max height).
fn screencast((quality, width, height): (u8, u32, u32)) -> Value {
    json!({
        "format": "jpeg",
        "quality": quality,
        "maxWidth": width,
        "maxHeight": height,
        "everyNthFrame": 1,
    })
}

/// The browser no longer waits on devices: count how long it did, and look
/// for a still moment from now on.
fn end_hold(st: &mut TabState) {
    if let Some(t) = st.held_since.take() {
        st.motion.held += t.elapsed();
    }
    let quiet = if st.motion.moving {
        SETTLE
    } else {
        SETTLE_LONE
    };
    st.motion.settle_at = Some(Instant::now() + quiet);
}

/// Move the devices watching `from` to `to` (a popup opened or closed).
fn move_viewers(from: &Arc<Tab>, to: &Arc<Tab>) -> Vec<u64> {
    let moved = from.viewers();
    for (id, ..) in &moved {
        from.detach(*id);
    }
    for (id, view, takes) in &moved {
        to.attach(*id, *view, *takes);
    }
    moved.into_iter().map(|(id, ..)| id).collect()
}

/// The type of a small image from its first bytes.
fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if b.starts_with(&[0, 0, 1, 0]) {
        Some("image/x-icon")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.starts_with(&[0xFF, 0xD8]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewports_are_clamped_and_frames_capped() {
        let v = Viewport::clamped(10, 99_999, f64::NAN, false);
        assert_eq!((v.width, v.height, v.dpr), (MIN_W, MAX_H, 1.0));
        // A phone: 412×915 CSS at 3x is reduced to stay under the caps.
        let phone = Viewport::clamped(412, 915, 3.0, true);
        let d = phone.frame_dpr(3.0);
        assert!(d < 3.0 && d > 1.5, "{d}");
        assert!((915.0 * d) <= MAX_SIDE + 0.5);
        assert!(412.0 * d * 915.0 * d <= MAX_PIXELS + 1.0);
        // A desktop tile at 1x is untouched.
        assert_eq!(Viewport::clamped(1280, 800, 1.0, false).frame_dpr(2.0), 1.0);
        assert_eq!(Viewport::clamped(400, 800, 3.0, true).frame_dpr(2.0), 2.0);
        // A 4K tile at 2x is brought down to 2048 wide.
        let big = Viewport::clamped(3840, 2160, 2.0, false);
        assert!((3840.0 * big.frame_dpr(2.0) - 2048.0).abs() < 1.0);
    }

    #[test]
    fn still_pictures_match_the_device_within_caps() {
        let phone = Viewport::clamped(412, 915, 2.625, true);
        assert_eq!(phone.page_dpr(2.0), 2.625);
        assert_eq!(Viewport::clamped(1280, 800, 1.0, false).page_dpr(2.0), 1.0);
        assert_eq!(Viewport::clamped(1280, 800, 2.0, false).page_dpr(2.0), 2.0);
        // Never above 3, and a huge tile stays within the pixel cap.
        assert_eq!(
            Viewport::clamped(400, 600, 4.0, true).page_dpr(2.0),
            MAX_DPR
        );
        let big = Viewport::clamped(3840, 2160, 2.0, false);
        assert_eq!(big.page_dpr(2.0), 1.0);
    }

    #[test]
    fn the_window_follows_the_queueing_delay() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut v = ViewerState::new(Takes {
            pipelined: true,
            webp: true,
        });
        let sent = |frame, at, alone| Sent {
            frame,
            at,
            bytes: 50 << 10,
            still: false,
            alone,
        };
        // The first frame goes alone: its round trip is the connection's.
        assert!(v.has_room(50 << 10));
        v.in_flight.push_back(sent(1, ms(0), true));
        assert!(!v.has_room(50 << 10), "one at a time until measured");
        v.acked(1, ms(100));
        assert_eq!(v.rtt_min, 100.0);
        // Quick acks while frames wait for room: the window grows.
        let w0 = v.window;
        v.limited = true;
        v.in_flight.push_back(sent(2, ms(200), true));
        v.acked(2, ms(305));
        assert!(v.window > w0, "{} > {w0}", v.window);
        // A frame that took far longer than that: it shrinks.
        let w1 = v.window;
        v.in_flight.push_back(sent(3, ms(500), false));
        v.acked(3, ms(700));
        assert!(v.window < w1, "{} < {w1}", v.window);
        // Without `pipelined`, one frame at a time.
        let mut one = ViewerState::new(Takes::default());
        one.in_flight.push_back(sent(1, ms(0), true));
        one.acked(1, ms(10));
        one.in_flight.push_back(sent(2, ms(20), true));
        assert!(!one.has_room(1));
    }

    #[test]
    fn big_frames_keep_a_slow_link_busy() {
        // 100 KB frames on a link that carries 1000 KB a second (100 ms
        // each), 150 ms round trip for a frame on its own.
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let sent = |frame, at, alone| Sent {
            frame,
            at,
            bytes: 100 << 10,
            still: false,
            alone,
        };
        let pipelined = Takes {
            pipelined: true,
            webp: true,
        };
        let mut v = ViewerState::new(pipelined);
        v.in_flight.push_back(sent(1, ms(0), true));
        v.acked(1, ms(150));
        assert_eq!(v.rate(), 0.0, "nothing to tell the link's speed by yet");
        // Two on their way, however small the window.
        v.window = MIN_WINDOW;
        v.in_flight.push_back(sent(2, ms(200), true));
        assert!(v.has_room(100 << 10));
        v.in_flight.push_back(sent(3, ms(200), false));
        assert!(!v.has_room(100 << 10), "the window holds back a third");
        // The second arrives one frame's time after the first: that is
        // how fast the link is, and waiting that long isn't a slowdown.
        v.acked(2, ms(350));
        let w = v.window;
        v.acked(3, ms(450));
        assert!((v.rate() - 1024.0 * 1000.0).abs() < 1.0, "{}", v.rate());
        assert!((v.send_ms(100 << 10) - 100.0).abs() < 1e-6);
        assert_eq!(v.window, w, "not shrunk for a frame's own time");
        // Without knowing that, the same wait shrinks the window.
        let mut blind = ViewerState::new(pipelined);
        blind.in_flight.push_back(sent(1, ms(0), true));
        blind.acked(1, ms(150));
        let w = blind.window;
        blind.in_flight.push_back(sent(2, ms(300), false));
        blind.acked(2, ms(550));
        assert!(blind.window < w);
        // A frame that waits far longer than one ahead of it still does.
        v.window = 256 << 10;
        v.in_flight.push_back(sent(4, ms(600), false));
        v.acked(4, ms(1100));
        assert!(v.window < 256 << 10);
        // Acks bunched together say nothing about the link.
        let mut b = ViewerState::new(pipelined);
        b.in_flight.push_back(sent(1, ms(0), true));
        b.acked(1, ms(150));
        b.in_flight.push_back(sent(2, ms(160), true));
        b.in_flight.push_back(sent(3, ms(160), false));
        b.acked(2, ms(300));
        b.acked(3, ms(302));
        assert_eq!(b.rate(), 0.0);
    }

    #[test]
    fn sharp_screens_lose_quality_before_size() {
        let phone = Viewport::clamped(412, 915, 2.625, true);
        let desk = Viewport::clamped(1280, 800, 1.0, false);
        let (steps, sizes) = ladder(&phone);
        assert!(steps[..4].iter().all(|s| s.0 == 1.0), "{steps:?}");
        assert!(steps[..4].windows(2).all(|w| w[1].1 < w[0].1));
        assert_eq!(ladder(&desk).0[3].0, 0.75);
        for s in [sizes, ladder(&desk).1] {
            assert!(s.windows(2).all(|w| w[1] < w[0]), "{s:?}");
        }
    }

    #[test]
    fn small_images_are_recognised() {
        assert_eq!(sniff_image(b"\x89PNG\r\n"), Some("image/png"));
        assert_eq!(sniff_image(&[0, 0, 1, 0, 1]), Some("image/x-icon"));
        assert_eq!(sniff_image(b"<html>"), None);
    }
}

#[cfg(all(test, unix))]
#[tokio::test]
async fn an_over_cap_signin_is_warned_even_when_a_spare_page_is_shed_first() {
    use crate::browser::BrowserConfig;
    use crate::browser::{Live, Proc, process};
    let tmp = tempfile::tempdir().unwrap();
    let (_host, browsers) =
        crate::browser::tests::fixture_browsers(tmp.path(), BrowserConfig::default());
    let b = browsers.server();
    let signin = browsers
        .new_agent_tab("signin-chat", "https://signin.invalid/", None)
        .unwrap();
    let spare = browsers
        .new_agent_tab("spare-chat", "https://spare.invalid/", None)
        .unwrap();
    let (cdp, _, events) = Cdp::channel();
    let live = Arc::new(Live {
        cdp: cdp.clone(),
        generation: 1,
        headless: true,
        major: "155".into(),
        started: Instant::now(),
        stopped: tokio_util::sync::CancellationToken::new(),
    });
    *b.0.live.write() = Some(live.clone());
    for (tab, age) in [(&signin, Duration::ZERO), (&spare, Duration::from_secs(31))] {
        let mut st = tab.st.lock();
        st.link = Some(Link {
            cdp: cdp.clone(),
            generation: 1,
            target: tab.key(),
            session: tab.key(),
        });
        st.linked_gen = Some(1);
        st.last_used = Instant::now() - age;
    }
    signin.attach(
        17,
        Viewport::clamped(1024, 700, 1.0, false),
        Takes::default(),
    );
    let mut notices = b.0.events().subscribe();
    b.memory_high(&serde_json::json!({"used": 101, "cap": 100, "over": true}));
    assert!(signin.is_live(), "active sign-in is not the shed page");
    assert!(!spare.is_live(), "spare page is shed");
    let mut warned = std::collections::HashSet::new();
    while let Ok(Event::Notice {
        session_id: Some(chat),
        message,
        ..
    }) = notices.try_recv()
    {
        assert!(
            message.contains("close to the memory"),
            "warning not a late crash notice"
        );
        warned.insert(chat);
    }
    assert!(warned.contains("signin-chat") && warned.contains("spare-chat"));
    // Shedding may not reduce memory. A later stop still happens; its
    // interruption must follow, not replace, the sign-in's warning.
    let child = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    *b.0.run.lock().await = Some(Proc::Local(process::Process {
        cdp: cdp.clone(),
        child: Some(child),
        log: Arc::new(Mutex::new(Default::default())),
        started: Instant::now(),
        lease: Some(process::ExitLease::new(
            None,
            tokio_util::sync::CancellationToken::new(),
        )),
    }));
    // The follow-up sample is still over cap after the spare is gone.
    b.memory_high(&serde_json::json!({"used": 101, "cap": 100, "over": false}));
    let end = tokio::spawn(b.clone().events_loop(live, events));
    cdp.close();
    let late = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Ok(Event::Notice {
                session_id: Some(chat),
                message,
                ..
            }) = notices.recv().await
                && chat == "signin-chat"
                && message.contains("stopped unexpectedly")
            {
                break message;
            }
        }
    })
    .await
    .unwrap();
    assert!(late.contains("Anything typed and not submitted is gone"));
    // The warning/shutdown ordering is fully synthetic; no Chrome launch
    // or desktop is needed when the recovery loop wakes.
    signin.detach(17);
    tokio::time::timeout(Duration::from_secs(5), end)
        .await
        .unwrap()
        .unwrap();
}

#[cfg(test)]
#[tokio::test]
async fn shedding_protects_long_agent_actions_and_fresh_popup_chains_and_revalidates() {
    let tmp = tempfile::tempdir().unwrap();
    let (_host, browsers) =
        crate::browser::tests::fixture_browsers(tmp.path(), super::BrowserConfig::default());
    let b = browsers.server();
    let base = browsers
        .new_agent_tab("chat", "https://fixture.invalid/", None)
        .unwrap();
    let (cdp, _out, _) = Cdp::channel();
    let link = |target: &str| Link {
        cdp: cdp.clone(),
        target: target.into(),
        session: target.into(),
        generation: 1,
    };
    base.with_state(|st| {
        st.link = Some(link("base"));
        st.last_used = Instant::now() - Duration::from_secs(900);
    });
    let acting = base.acting().await;
    // Even after a long action's last-use stamp ages beyond the idle window.
    base.with_state(|st| st.last_used = Instant::now() - Duration::from_secs(900));
    assert!(base.memory_use().0);
    b.memory_high(&json!({"used": 101, "cap": 100, "over": true}));
    assert!(base.is_live() && !base.shed(Duration::ZERO, "must not interrupt active action"));
    drop(acting);
    let popup = Tab::new(
        &b.0,
        base.key(),
        None,
        Some("chat".into()),
        Some("chat".into()),
        "https://popup.invalid/".into(),
    );
    popup.with_state(|st| st.link = Some(link("popup")));
    *popup.parent.lock() = Arc::downgrade(&base);
    *base.child.lock() = Some(popup.clone());
    base.with_state(|st| st.has_popup = true);
    assert!(base.memory_use().1.elapsed() < Duration::from_secs(30));
    b.memory_high(&json!({"used": 101, "cap": 100, "over": true}));
    assert!(base.is_live() && popup.is_live());
    popup.with_state(|st| st.last_used = Instant::now() - Duration::from_secs(900));
    assert!(!base.memory_use().0);
    // A candidate was picked, then a popup action/viewer arrived.
    let popup_acting = popup.acting().await;
    assert!(!base.shed(Duration::ZERO, "selected earlier"));
    drop(popup_acting);
    popup.attach(23, Viewport::default(), Takes::default());
    assert!(!base.shed(Duration::ZERO, "selected earlier"));
    popup.detach(23);
    popup.with_state(|st| st.last_used = Instant::now() - Duration::from_secs(900));
    assert!(base.shed(Duration::from_secs(30), "shed old chain"));
    assert!(!base.is_live() && !popup.is_live() && base.child.lock().is_none());
    assert_eq!(base.take_interrupted().as_deref(), Some("shed old chain"));
}

#[cfg(test)]
#[tokio::test(start_paused = true)]
async fn low_memory_refuses_when_all_protected_warns_once_and_never_closes_browser() {
    let tmp = tempfile::tempdir().unwrap();
    let (_host, browsers) =
        crate::browser::tests::fixture_browsers(tmp.path(), super::BrowserConfig::default());
    let b = browsers.server();
    let (cdp, _out, _) = Cdp::channel();
    let live = Arc::new(Live {
        cdp: cdp.clone(),
        generation: 1,
        headless: true,
        major: "155".into(),
        started: Instant::now(),
        stopped: tokio_util::sync::CancellationToken::new(),
    });
    *b.0.live.write() = Some(live.clone());
    let tabs: Vec<_> = (0..2)
        .map(|_| {
            browsers
                .new_agent_tab("protected-chat", "https://fixture.invalid/", None)
                .unwrap()
        })
        .collect();
    for tab in &tabs {
        tab.with_state(|st| {
            st.link = Some(Link {
                cdp: cdp.clone(),
                target: tab.key(),
                session: tab.key(),
                generation: 1,
            });
            st.linked_gen = Some(1);
            st.last_used = Instant::now() - Duration::from_secs(900);
            st.agent_since = Some(Instant::now() - Duration::from_secs(900));
        });
    }
    let new = browsers
        .new_agent_tab("new-chat", "https://new.invalid/", None)
        .unwrap();
    let mut notices = b.0.events().subscribe();
    for _ in 0..3 {
        assert_eq!(
            b.make_room_sample(Some(&new.key()), true).err().unwrap(),
            super::LOW_MEMORY
        );
    }
    let mut warned = std::collections::HashSet::new();
    while let Ok(Event::Notice {
        session_id: chat,
        message,
        ..
    }) = notices.try_recv()
    {
        assert_eq!(message, super::LOW_MEMORY);
        assert!(warned.insert(chat), "one warning per chat per episode");
    }
    assert_eq!(warned.len(), 2);
    assert!(
        tabs.iter()
            .all(|t| t.is_live() && t.take_interrupted().is_none())
    );
    assert!(
        !new.is_live() && !cdp.is_closed() && Arc::ptr_eq(b.0.live.read().as_ref().unwrap(), &live)
    );
    b.pressure_sample(false, &tabs, None);
    tokio::time::advance(Duration::from_secs(30)).await;
    b.pressure_sample(false, &tabs, None);
    assert_eq!(
        b.make_room_sample(Some(&new.key()), true).err().unwrap(),
        super::LOW_MEMORY
    );
    assert!(
        matches!(notices.try_recv(), Ok(Event::Notice { .. })),
        "healthy sample resets episode"
    );
}

#[cfg(test)]
#[tokio::test]
async fn low_memory_admission_refuses_a_candidate_that_fails_final_revalidation() {
    let tmp = tempfile::tempdir().unwrap();
    let (_host, browsers) =
        crate::browser::tests::fixture_browsers(tmp.path(), super::BrowserConfig::default());
    let b = browsers.server();
    let (cdp, _out, _) = Cdp::channel();
    *b.0.live.write() = Some(Arc::new(Live {
        cdp: cdp.clone(),
        generation: 1,
        headless: true,
        major: "155".into(),
        started: Instant::now(),
        stopped: tokio_util::sync::CancellationToken::new(),
    }));
    let spare = browsers
        .new_agent_tab("signin", "https://fixture.invalid/", None)
        .unwrap();
    spare.with_state(|st| {
        st.link = Some(Link {
            cdp: cdp.clone(),
            target: spare.key(),
            session: spare.key(),
            generation: 1,
        });
        st.linked_gen = Some(1);
        st.last_used = Instant::now() - Duration::from_secs(900);
    });
    assert!(!spare.memory_use().0); // selected while spare
    let action = spare.acting().await;
    assert_eq!(
        super::require_pressure_room(Some(&spare), Duration::ZERO)
            .err()
            .unwrap(),
        super::LOW_MEMORY
    );
    drop(action);
    spare.attach(31, Viewport::default(), Takes::default());
    assert_eq!(
        super::require_pressure_room(Some(&spare), Duration::ZERO)
            .err()
            .unwrap(),
        super::LOW_MEMORY
    );
    assert!(spare.is_live() && spare.take_interrupted().is_none() && !cdp.is_closed());
    spare.detach(31);
    assert!(super::require_pressure_room(Some(&spare), Duration::ZERO).is_ok());
    assert!(
        !spare.is_live() && !cdp.is_closed(),
        "only an unused page is shed, never its browser"
    );
}

#[cfg(test)]
mod r5_admission_tests {
    use super::*;

    fn page(tab: &Arc<Tab>, cdp: &Arc<Cdp>) {
        tab.with_state(|st| {
            st.link = Some(Link {
                cdp: cdp.clone(),
                target: tab.key(),
                session: tab.key(),
                generation: 1,
            });
            st.linked_gen = Some(1);
            st.last_used = Instant::now() - Duration::from_secs(900);
        });
    }

    fn running(b: &Browser, cdp: &Arc<Cdp>) {
        *b.0.live.write() = Some(Arc::new(Live {
            cdp: cdp.clone(),
            generation: 1,
            headless: true,
            major: "155".into(),
            started: Instant::now(),
            stopped: tokio_util::sync::CancellationToken::new(),
        }));
    }

    #[tokio::test]
    async fn retained_handle_is_refused_at_actual_create_while_live_pages_remain_usable() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 2,
                ..Default::default()
            },
        );
        let b = browsers.server();
        let (cdp, mut out, _) = Cdp::channel();
        running(b, &cdp);
        let old: Vec<_> = (0..2)
            .map(|_| {
                b.new_agent_tab("old-chat", "https://fixture.invalid/")
                    .unwrap()
            })
            .collect();
        for t in &old {
            page(t, &cdp);
            t.attach(9, Viewport::default(), Takes::default());
        }
        let retained = b
            .new_agent_tab("requesting-chat", "https://new.invalid/")
            .unwrap();
        assert!(retained.ensure_live().await.is_err());
        assert!(
            out.try_recv().is_err(),
            "refusal precedes createTarget or any destructive CDP"
        );
        for t in &old {
            assert!(t.ensure_live().await.is_ok() && t.take_interrupted().is_none());
        }
        assert!(!retained.is_live() && !cdp.is_closed());
    }

    #[tokio::test(start_paused = true)]
    async fn simultaneous_page_opens_cannot_both_take_the_same_headroom() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 2,
                ..Default::default()
            },
        );
        let b = browsers.server();
        let (cdp, _, _) = Cdp::channel();
        running(b, &cdp);
        let old = b.new_agent_tab("old", "https://old.invalid/").unwrap();
        page(&old, &cdp);
        old.attach(9, Viewport::default(), Takes::default());
        let first = b.new_agent_tab("first", "https://first.invalid/").unwrap();
        let second = b
            .new_agent_tab("second", "https://second.invalid/")
            .unwrap();
        let guard = b.admit_page_sample(Some(&first), || false).await.unwrap();
        let browser = b.clone();
        let tab = second.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            ready.send(()).unwrap();
            browser.admit_page_sample(Some(&tab), || false).await
        });
        started.await.unwrap();
        assert!(
            !pending.is_finished(),
            "second opening is serialized, not pre-admitted"
        );
        page(&first, &cdp);
        first.attach(10, Viewport::default(), Takes::default());
        drop(guard);
        assert!(
            pending.await.unwrap().is_err(),
            "first publication consumed the only free slot"
        );
        assert!(old.is_live() && first.is_live() && !second.is_live());
    }

    #[tokio::test]
    async fn refused_registered_requester_is_warned_once_and_not_shed() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let (cdp, _, _) = Cdp::channel();
        running(b, &cdp);
        let requester = Tab::new(
            &b.0,
            "new-requester".into(),
            None,
            Some("new-chat".into()),
            None,
            "https://fixture.invalid/".into(),
        );
        b.0.tabs.lock().insert(requester.key(), requester.clone());
        let mut notices = b.0.events().subscribe();
        for _ in 0..3 {
            assert_eq!(
                b.admit_page_sample(Some(&requester), || true)
                    .await
                    .err()
                    .unwrap(),
                super::super::LOW_MEMORY
            );
        }
        assert!(
            matches!(notices.try_recv(), Ok(Event::Notice { session_id: Some(chat), .. }) if chat == "new-chat")
        );
        assert!(
            notices.try_recv().is_err(),
            "one warning for the requesting chat"
        );
        page(&requester, &cdp);
        assert!(
            b.admit_page_sample(Some(&requester), || true)
                .await
                .is_err()
        );
        assert!(
            requester.is_live() && requester.take_interrupted().is_none(),
            "requester excluded from shedding"
        );
    }

    #[tokio::test]
    async fn all_protected_pages_and_handles_survive_refusal_at_the_per_chat_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let (cdp, mut out, _) = Cdp::channel();
        running(b, &cdp);
        let old: Vec<_> = (0..6)
            .map(|_| {
                b.new_agent_tab("signin-chat", "https://fixture.invalid/")
                    .unwrap()
            })
            .collect();
        for t in &old {
            page(t, &cdp);
            t.attach(9, Viewport::default(), Takes::default());
        }
        assert!(
            b.new_agent_tab("signin-chat", "https://new.invalid/")
                .is_err()
        );
        assert_eq!(b.own_tabs("signin-chat").len(), old.len());
        for t in old {
            assert!(Arc::ptr_eq(&b.find(&t.key()).unwrap(), &t));
            assert!(t.is_live() && t.take_interrupted().is_none());
            assert!(b.why_closed(&t.key(), "signin-chat").is_none());
        }
        assert!(
            out.try_recv().is_err() && !cdp.is_closed(),
            "no old page was closed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn warning_hysteresis_does_not_flap_or_block_current_healthy_admission() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let tab = b
            .new_agent_tab("new-chat", "https://fixture.invalid/")
            .unwrap();
        let mut notices = b.0.events().subscribe();
        assert!(b.admit_page_sample(Some(&tab), || true).await.is_err());
        assert!(matches!(notices.try_recv(), Ok(Event::Notice { .. })));
        // A sample just above the low mark admits pages immediately, even
        // while the previous warning episode is deliberately retained.
        drop(b.admit_page_sample(Some(&tab), || false).await.unwrap());
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(b.admit_page_sample(Some(&tab), || true).await.is_err());
        assert!(
            notices.try_recv().is_err(),
            "one healthy fluctuation does not re-warn"
        );
        drop(b.admit_page_sample(Some(&tab), || false).await.unwrap());
        tokio::time::advance(Duration::from_secs(29)).await;
        drop(b.admit_page_sample(Some(&tab), || false).await.unwrap());
        assert!(!b.0.pressure_warned.lock().chats.is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        drop(b.admit_page_sample(Some(&tab), || false).await.unwrap());
        assert!(b.0.pressure_warned.lock().chats.is_empty());
        assert!(b.admit_page_sample(Some(&tab), || true).await.is_err());
        assert!(
            matches!(notices.try_recv(), Ok(Event::Notice { .. })),
            "30s recovery permits a new episode"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_admission_samples_pressure_only_after_it_gets_the_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let tab = b.new_agent_tab("chat", "https://fixture.invalid/").unwrap();
        let guard = b.0.page_admission.clone().lock_owned().await;
        let low = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let current = low.clone();
        let browser = b.clone();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let admission = tokio::spawn(async move {
            entered.send(()).unwrap();
            browser
                .admit_page_sample(Some(&tab), || {
                    current.load(std::sync::atomic::Ordering::SeqCst)
                })
                .await
        });
        waiting.await.unwrap();
        low.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(guard);
        assert_eq!(
            admission.await.unwrap().err().unwrap(),
            super::super::LOW_MEMORY,
            "not a stale healthy decision made before queueing"
        );
    }

    #[test]
    fn repeated_handle_allocation_without_navigation_is_bounded_and_names_tabs_to_close() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let tabs: Vec<_> = (0..6)
            .map(|_| b.new_agent_tab("chat", "about:blank").unwrap())
            .collect();
        for _ in 0..100 {
            let error = b.new_agent_tab("chat", "about:blank").err().unwrap();
            assert!(error.contains("This chat has 6 tabs") && error.contains("Close one"));
            for tab in &tabs {
                assert!(error.contains(&format!("`{}`", tab.key())));
            }
            assert_eq!(b.own_tabs("chat").len(), 6);
        }
        assert!(tabs.iter().all(|t| !t.is_live() && t.registered()));
    }

    #[tokio::test]
    async fn repeated_admission_refusals_remove_each_new_handle_without_closing_old_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 2,
                ..Default::default()
            },
        );
        let b = browsers.server();
        let (cdp, mut out, _) = Cdp::channel();
        running(b, &cdp);
        let old: Vec<_> = (0..2)
            .map(|_| b.new_agent_tab("chat", "about:blank").unwrap())
            .collect();
        for tab in &old {
            page(tab, &cdp);
            tab.attach(9, Viewport::default(), Takes::default());
        }
        for _ in 0..20 {
            let tab = b.new_agent_tab("chat", "about:blank").unwrap();
            assert!(tab.ensure_live().await.is_err());
            assert!(b.find(&tab.key()).is_none() && !tab.registered());
            assert_eq!(b.own_tabs("chat").len(), 2);
        }
        let canceled = b.new_agent_tab("chat", "about:blank").unwrap();
        let guard = b.0.page_admission.clone().lock_owned().await;
        let pending_tab = canceled.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            entered.send(()).unwrap();
            pending_tab.ensure_live().await
        });
        ready.await.unwrap();
        assert!(canceled.open.try_lock().is_err());
        pending.abort();
        assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
        drop(guard);
        assert!(!canceled.registered() && b.own_tabs("chat").len() == 2);
        assert!(
            old.iter()
                .all(|t| t.is_live() && t.registered() && t.take_interrupted().is_none())
        );
        assert!(
            out.try_recv().is_err(),
            "refusal never creates or closes a Chrome page"
        );
    }

    #[tokio::test]
    async fn racing_same_owner_creation_keeps_registry_and_real_cdp_page_count_in_agreement() {
        if super::super::process::find(None).is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 6,
                virtual_screens: false,
                // Admission/page-count fixtures must not race the temporary
                // speed-probe page; rendering speed is tested separately.
                speed: Some(60.0),
                ..Default::default()
            },
        );
        let b = browsers.server();
        let live = b.start().await.unwrap();
        let page_count = |value: Value| {
            value["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["type"] == "page")
                .count()
        };
        let initial = page_count(
            live.cdp
                .call(None, "Target.getTargets", json!({}))
                .await
                .unwrap(),
        );
        let mut old = Vec::new();
        for _ in 0..4 {
            let tab = b.new_agent_tab("chat", "about:blank").unwrap();
            tab.ensure_live().await.unwrap();
            tab.attach(9, Viewport::default(), Takes::default());
            old.push(tab);
        }
        let first = b.new_agent_tab("chat", "about:blank").unwrap();
        let second = b.new_agent_tab("chat", "about:blank").unwrap();
        let guard = b.0.page_admission.clone().lock_owned().await;
        let (entered, mut ready) = mpsc::unbounded_channel();
        let a = first.clone();
        let started = entered.clone();
        let opening_a = tokio::spawn(async move {
            started.send(()).unwrap();
            a.ensure_live().await
        });
        let c = second.clone();
        let opening_b = tokio::spawn(async move {
            entered.send(()).unwrap();
            c.ensure_live().await
        });
        ready.recv().await.unwrap();
        ready.recv().await.unwrap();
        assert!(first.open.try_lock().is_err() && second.open.try_lock().is_err());
        assert!(first.registered() && second.registered());
        assert!(
            b.new_agent_tab("chat", "about:blank").is_err(),
            "a waiting creation is never retired to allocate another handle"
        );
        assert!(b.close_own_tab("chat", &first.key()));
        drop(guard);
        assert!(
            opening_a.await.unwrap().is_err(),
            "removed requester refuses before createTarget"
        );
        opening_b.await.unwrap().unwrap();
        assert!(!first.registered() && second.registered() && second.is_live());
        assert_eq!(b.own_tabs("chat").len(), 5);
        assert!(old.iter().all(|t| t.registered() && t.is_live()));
        let roots = b.0.tabs.lock().values().filter(|t| t.is_live()).count();
        assert_eq!(roots, 5);
        assert_eq!(
            page_count(
                live.cdp
                    .call(None, "Target.getTargets", json!({}))
                    .await
                    .unwrap()
            ),
            initial + roots,
            "real Chromium has no unregistered page from the removed request"
        );
        b.stop().await;
    }

    #[tokio::test]
    async fn vanished_counted_root_with_surviving_popup_refuses_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig::default(),
        );
        let b = browsers.server();
        let (cdp, mut out, _) = Cdp::channel();
        running(b, &cdp);
        let root = b.new_agent_tab("chat", "about:blank").unwrap();
        page(&root, &cdp);
        let popup = Tab::new(
            &b.0,
            root.key(),
            None,
            Some("chat".into()),
            None,
            "about:blank".into(),
        );
        page(&popup, &cdp);
        *root.child.lock() = Some(popup.clone());
        *popup.parent.lock() = Arc::downgrade(&root);
        let requester = b.new_agent_tab("chat", "about:blank").unwrap();
        let _guard = b.0.page_admission.clone().lock_owned().await;
        let plan = b
            .plan_page(Some(&requester.key()), Some(&requester), true)
            .unwrap();
        assert_eq!(plan.victims.len(), 1);
        root.with_state(|st| st.link = None);
        assert!(popup.is_live() && !root.is_live());
        assert!(b.commit_page_plan(plan).is_err());
        assert!(
            root.registered()
                && popup.is_live()
                && root
                    .child
                    .lock()
                    .as_ref()
                    .is_some_and(|t| Arc::ptr_eq(t, &popup))
        );
        assert!(root.take_interrupted().is_none() && popup.take_interrupted().is_none());
        assert!(
            out.try_recv().is_err(),
            "surviving popup cannot pay for the vanished root twice"
        );
    }

    #[tokio::test]
    async fn all_pressure_and_headroom_victims_revalidate_before_any_closure() {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 3,
                ..Default::default()
            },
        );
        let b = browsers.server();
        let (cdp, mut out, _) = Cdp::channel();
        running(b, &cdp);
        let old: Vec<_> = (0..4)
            .map(|_| b.new_agent_tab("chat", "about:blank").unwrap())
            .collect();
        for (i, t) in old.iter().enumerate() {
            page(t, &cdp);
            if i >= 2 {
                t.attach(9, Viewport::default(), Takes::default());
            }
        }
        let requester = b.new_agent_tab("chat", "about:blank").unwrap();
        let _guard = b.0.page_admission.clone().lock_owned().await;
        let plan = b
            .plan_page(Some(&requester.key()), Some(&requester), true)
            .unwrap();
        assert_eq!(plan.victims.len(), 2);
        plan.victims[1]
            .tab
            .attach(77, Viewport::default(), Takes::default());
        assert!(b.commit_page_plan(plan).is_err());
        assert!(
            old.iter()
                .all(|t| t.registered() && t.is_live() && t.take_interrupted().is_none())
        );
        assert!(
            out.try_recv().is_err() && !cdp.is_closed(),
            "no partial eviction or create"
        );
    }

    #[tokio::test]
    async fn canceled_create_rolls_back_before_next_admission_with_real_chromium() {
        if super::super::process::find(None).is_none() {
            eprintln!(
                "SKIP canceled_create_rolls_back_before_next_admission_with_real_chromium: no Chromium executable found"
            );
            return;
        }
        fn page_ids(value: Value) -> std::collections::HashSet<String> {
            value["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| t["type"] == "page")
                .map(|t| t["targetId"].as_str().unwrap().to_string())
                .collect()
        }
        // First cancel after claim with the first initialization ACK delayed.
        // Then cancel with a real create response still held: its late target
        // id must remain owned by rollback, rather than escaping admission.
        for method in ["Page.setWebLifecycleState", "Target.createTarget"] {
            let tmp = tempfile::tempdir().unwrap();
            let (_host, browsers) = crate::browser::tests::fixture_browsers(
                tmp.path(),
                super::super::BrowserConfig {
                    max_tabs: 2,
                    virtual_screens: false,
                    speed: Some(60.0),
                    ..Default::default()
                },
            );
            let b = browsers.server();
            let live = b.start().await.unwrap();
            let cdp = live.cdp.clone();
            let initial = page_ids(
                cdp.call(None, "Target.getTargets", json!({}))
                    .await
                    .unwrap(),
            );
            let old = b.new_agent_tab("chat", "about:blank").unwrap();
            let old_link = old.ensure_live().await.unwrap();
            old.attach(9, Viewport::default(), Takes::default());
            let before = page_ids(
                cdp.call(None, "Target.getTargets", json!({}))
                    .await
                    .unwrap(),
            );
            assert_eq!(before.len(), initial.len() + 1);
            let canceled = b.new_agent_tab("chat", "about:blank").unwrap();
            let (reached, release) = cdp.delay_next_reply(method, Some(&old_link.session));
            let tab = canceled.clone();
            let opening = tokio::spawn(async move { tab.ensure_live().await });
            tokio::time::timeout(Duration::from_secs(30), reached)
                .await
                .unwrap()
                .unwrap();
            let during = page_ids(
                cdp.call(None, "Target.getTargets", json!({}))
                    .await
                    .unwrap(),
            );
            let target = during.difference(&before).next().unwrap().clone();
            assert_eq!(
                during.len(),
                before.len() + 1,
                "the canceled page really exists"
            );
            assert!(!canceled.is_live(), "link has not been published");
            if method == "Page.setWebLifecycleState" {
                assert!(
                    !b.0.attached.lock().contains_key(&target),
                    "the target has been claimed before cancellation"
                );
            }
            // Closing the real target is not enough: hold the close ACK to
            // prove the owner keeps admission through confirmed rollback.
            let (closed, acknowledge_close) = cdp.delay_next_reply("Target.closeTarget", None);
            opening.abort();
            assert!(matches!(opening.await, Err(error) if error.is_cancelled()));
            assert!(!canceled.registered() && b.own_tabs("chat").len() == 1);
            assert!(
                b.0.page_admission.try_lock().is_err(),
                "rollback owns admission after cancellation"
            );
            let next = b.new_agent_tab("chat", "about:blank").unwrap();
            let tab = next.clone();
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let opening_next = tokio::spawn(async move {
                entered.send(()).unwrap();
                tab.ensure_live().await
            });
            waiting.await.unwrap();
            assert!(next.open.try_lock().is_err() && !next.is_live());
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(30), closed)
                .await
                .unwrap()
                .unwrap();
            assert!(
                b.0.page_admission.try_lock().is_err() && !next.is_live(),
                "next create still waits for rollback ACK"
            );
            acknowledge_close.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(30), opening_next)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let after = page_ids(
                cdp.call(None, "Target.getTargets", json!({}))
                    .await
                    .unwrap(),
            );
            let roots = b.0.tabs.lock().values().filter(|t| t.is_live()).count();
            assert_eq!(roots, 2);
            assert_eq!(b.own_tabs("chat").len(), roots);
            assert!(old.registered() && old.is_live() && next.registered() && next.is_live());
            assert!(!canceled.registered() && !after.contains(&target));
            assert!(
                before.is_subset(&after),
                "rollback closed only its exact new target"
            );
            assert_eq!(
                after.len(),
                initial.len() + roots,
                "real CDP page count matches registered live roots"
            );
            assert!(b.0.page_admission.try_lock().is_ok());
            b.stop().await;
        }
    }

    async fn rollback_deadline_case(stall: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let (_host, browsers) = crate::browser::tests::fixture_browsers(
            tmp.path(),
            super::super::BrowserConfig {
                max_tabs: 2,
                virtual_screens: false,
                ..Default::default()
            },
        );
        let b = browsers.server();
        let (cdp, mut out, events) = Cdp::channel();
        running(b, &cdp);
        let original = b.0.live.read().clone().unwrap();
        let exit = tokio_util::sync::CancellationToken::new();
        let (killed, stopped) = tokio::sync::oneshot::channel();
        *b.0.run.lock().await = Some(super::super::Proc::Fixture {
            cdp: cdp.clone(),
            exit: exit.clone(),
            killed: Some(killed),
        });
        let event_loop = tokio::spawn(b.clone().events_loop(original.clone(), events));
        let abandoned = b.new_agent_tab("chat", "about:blank").unwrap();
        let admission = b.0.page_admission.clone().lock_owned().await;
        let mut owner = PageCreation::new(
            abandoned.clone(),
            original.clone(),
            admission,
            json!({"url":"about:blank"}),
        );
        let request: Value = serde_json::from_slice(&out.recv().await.unwrap()).unwrap();
        assert_eq!(request["method"], "Target.createTarget");
        if stall != "create" {
            cdp.feed(
                json!({"id":request["id"],"result":{"targetId":"abandoned"}})
                    .to_string()
                    .as_bytes(),
            );
            assert_eq!(owner.target().await.unwrap(), "abandoned");
        }
        drop(owner);
        b.close_own_tab("chat", &abandoned.key());
        if stall != "create" {
            let close: Value = serde_json::from_slice(&out.recv().await.unwrap()).unwrap();
            assert_eq!(close["method"], "Target.closeTarget");
            assert_eq!(close["params"]["targetId"], "abandoned");
            if stall == "absence" {
                cdp.feed(
                    json!({"id":close["id"],"result":{"success":false}})
                        .to_string()
                        .as_bytes(),
                );
                let absent: Value = serde_json::from_slice(&out.recv().await.unwrap()).unwrap();
                assert_eq!(absent["method"], "Target.getTargets");
                // The absence check never answers either.
            }
        }
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(!cdp.is_closed() && !exit.is_cancelled() && !original.stopped.is_cancelled());
        assert!(
            b.0.page_admission.try_lock().is_err(),
            "no early admission release"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        stopped.await.unwrap();
        assert!(
            cdp.is_closed(),
            "deadline enters existing generation recovery"
        );
        assert!(!original.stopped.is_cancelled());
        assert!(
            b.0.page_admission.try_lock().is_err(),
            "CDP EOF/kill request is not confirmed exit"
        );
        exit.cancel();
        original.stopped.cancelled().await;
        event_loop.await.unwrap();
        let admission = tokio::time::timeout(
            Duration::from_secs(1),
            b.0.page_admission.clone().lock_owned(),
        )
        .await
        .unwrap();
        drop(admission);
        assert!(!abandoned.registered());

        // A subsequent generation can create and publish a page only after
        // the scripted process/tree exit confirmed the old teardown.
        let (fresh, mut commands, events) = Cdp::channel();
        let next_live = Arc::new(Live {
            cdp: fresh.clone(),
            generation: 2,
            headless: true,
            major: "155".into(),
            started: Instant::now(),
            stopped: tokio_util::sync::CancellationToken::new(),
        });
        let next_exit = tokio_util::sync::CancellationToken::new();
        next_exit.cancel();
        *b.0.live.write() = Some(next_live.clone());
        *b.0.run.lock().await = Some(super::super::Proc::Fixture {
            cdp: fresh.clone(),
            exit: next_exit,
            killed: None,
        });
        let next_events = tokio::spawn(b.clone().events_loop(next_live.clone(), events));
        let old_stopped = original.stopped.clone();
        let wire = fresh.clone();
        let browser = b.clone();
        let (created, observed) = tokio::sync::oneshot::channel();
        let driver = tokio::spawn(async move {
            let mut created = Some(created);
            while let Some(raw) = commands.recv().await {
                let command: Value = serde_json::from_slice(&raw).unwrap();
                let result = if command["method"] == "Target.createTarget" {
                    assert!(
                        old_stopped.is_cancelled(),
                        "no create before confirmed old-generation teardown"
                    );
                    let _ = created.take().unwrap().send(());
                    browser.on_browser_event(&next_live, CdpEvent { method: "Target.attachedToTarget".into(), params: json!({"sessionId":"new-session","waitingForDebugger":true,"targetInfo":{"type":"page","targetId":"new-page"}}) });
                    json!({"targetId":"new-page"})
                } else {
                    json!({"identifier":"guard", "windowId":1, "bounds":{}, "executionContextId":1, "frameTree":{"frame":{"id":"main"}}, "result":{"type":"undefined"}})
                };
                wire.feed(
                    json!({"id":command["id"],"result":result})
                        .to_string()
                        .as_bytes(),
                );
            }
        });
        let next = b.new_agent_tab("chat", "about:blank").unwrap();
        next.ensure_live().await.unwrap();
        observed.await.unwrap();
        assert!(next.registered() && next.is_live() && !fresh.is_closed());
        assert_eq!(b.own_tabs("chat").len(), 1);
        b.stop().await;
        next_events.await.unwrap();
        driver.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn missing_create_reply_recovers_after_deadline_and_releases_only_after_exit() {
        rollback_deadline_case("create").await;
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_close_or_absence_recovers_after_deadline_and_releases_only_after_exit() {
        for stall in ["close", "absence"] {
            rollback_deadline_case(stall).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stale_creation_recovery_never_stops_a_different_cdp_or_generation() {
        for changed_cdp in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let (_host, browsers) = crate::browser::tests::fixture_browsers(
                tmp.path(),
                super::super::BrowserConfig::default(),
            );
            let b = browsers.server();
            let (old, _requests, _) = Cdp::channel();
            running(b, &old);
            let original = b.0.live.read().clone().unwrap();
            let (fresh, mut requests, _) = Cdp::channel();
            let replacement = Arc::new(Live {
                cdp: if changed_cdp {
                    fresh.clone()
                } else {
                    old.clone()
                },
                generation: if changed_cdp { 1 } else { 2 },
                headless: true,
                major: "155".into(),
                started: Instant::now(),
                stopped: tokio_util::sync::CancellationToken::new(),
            });
            *b.0.live.write() = Some(replacement.clone());
            *b.0.run.lock().await = Some(super::super::Proc::Fixture {
                cdp: replacement.cdp.clone(),
                exit: tokio_util::sync::CancellationToken::new(),
                killed: None,
            });
            let tab = b.new_agent_tab("chat", "about:blank").unwrap();
            let guard = b.0.page_admission.clone().lock_owned().await;
            drop(PageCreation::new(
                tab,
                original.clone(),
                guard,
                json!({"url":"about:blank"}),
            ));
            drop(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    b.0.page_admission.clone().lock_owned(),
                )
                .await
                .unwrap(),
            );
            b.recover_creation(&original).await;
            assert!(!replacement.cdp.is_closed() && !replacement.stopped.is_cancelled());
            assert!(Arc::ptr_eq(&b.0.live.read().clone().unwrap(), &replacement));
            assert!(
                requests.try_recv().is_err(),
                "stale rollback sends nothing to the replacement connection"
            );
            old.close();
            fresh.close();
        }
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
