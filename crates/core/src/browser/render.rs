//! How sharp a tab's moving pictures are: its render scale.
//!
//! Servers have no GPU, so Chromium draws every screencast frame in
//! software on one thread (the display compositor): the work grows with the
//! pixels drawn, and on CPUs without AVX2 (most virtual machines' generic
//! CPUs) each pixel costs about 2.5× more. One render scale for the whole
//! browser (`--force-device-scale-factor`) either draws 4× the pixels a 1x
//! screen needs or softens phones.
//!
//! So in headless mode each tab gets a window of its own on a virtual screen
//! (`Emulation.addScreen`) whose pixel ratio is that tab's render scale: a
//! 1x desktop tile draws at 1x, a phone at up to 2x. How sharp is chosen
//! per tab from `[browser] quality` and the server's speed, measured once
//! (per browser version and CPU) when the browser first starts: Auto picks
//! the sharpest scale that still draws a frame within `BUDGET_MS`. Browsers
//! without virtual screens get one scale for all tabs, picked the same way.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tessera_proto::BrowserQuality;

use super::Browser;
use super::cdp::Cdp;

/// Render scales tabs can have (pixel ratios of the virtual screens).
pub(crate) const STEPS: [f64; 9] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
/// The most a moving frame may take to draw (ms): Auto (about 50 a
/// second), Smooth (the least work).
const BUDGET_MS: f64 = 20.0;
const SMOOTH_MS: f64 = 10.0;
/// On a slow server, moving frames are drawn down to this scale (a sharp
/// picture follows once the page stops): Auto, Smooth.
const AUTO_MIN: f64 = 0.75;
const SMOOTH_MIN: f64 = 0.5;
/// At or below this many ms per megapixel a server draws a 4-megapixel
/// frame within one display frame: as fast as pages go. Above the second
/// it is slow (under 30 such frames a second).
const FAST_MS_PER_MPX: f64 = 4.6;
const SLOW_MS_PER_MPX: f64 = 1000.0 / (30.0 * PROBE_W * PROBE_H / 1e6);
/// What the speed is measured on: a 1280×800 page at 2x.
const PROBE_W: f64 = 2560.0;
const PROBE_H: f64 = 1600.0;
/// The tile Auto sizes the one scale for when tabs can't have their own.
const TYPICAL_MPX: f64 = 1280.0 * 800.0 / 1e6;

/// The largest step at or below `r`, at least `min`.
pub(crate) fn quantize(r: f64, min: f64) -> f64 {
    STEPS
        .iter()
        .rev()
        .copied()
        .find(|s| *s <= r.max(min) + 1e-6)
        .unwrap_or(min)
}

/// The smallest step at or above `r` (1–3): frames that need 1.6x are
/// drawn at 1.75x and scaled down, never up.
pub(crate) fn step_up(r: f64) -> f64 {
    STEPS
        .iter()
        .copied()
        .find(|s| *s >= r.max(1.0) - 1e-6)
        .unwrap_or(3.0)
}

/// The render scale for a tab: `need` is what its frames could use (the
/// device's pixel ratio times the zoom, within the caps), `css_mpx` the
/// page area it draws (megapixels at 1x), `ms_per_mpx` the server's
/// measured speed.
pub(crate) fn pick(
    quality: BrowserQuality,
    need: f64,
    css_mpx: f64,
    ms_per_mpx: Option<f64>,
) -> f64 {
    let need = if need.is_finite() { need } else { 1.0 };
    let full = step_up(need);
    // The sharpest scale that draws within `budget` ms (None: any).
    let fits = |budget: f64| match ms_per_mpx {
        Some(k) if k > FAST_MS_PER_MPX => Some((budget / (k * css_mpx.max(0.01))).sqrt()),
        Some(_) => None,
        // Not measured: CPUs with AVX2 draw fast enough.
        None if fast_cpu() => None,
        None => Some(1.0),
    };
    match quality {
        BrowserQuality::Sharp => full,
        BrowserQuality::Smooth => match fits(SMOOTH_MS) {
            Some(f) if f < 1.0 => quantize(f, SMOOTH_MIN),
            _ => 1.0,
        },
        BrowserQuality::Auto => match fits(BUDGET_MS) {
            Some(f) if f < full => quantize(need.min(f), AUTO_MIN),
            _ => full,
        },
    }
}

/// The one render scale for every tab (browsers without virtual screens).
pub(crate) fn global(quality: BrowserQuality, cap: f64, ms_per_mpx: Option<f64>) -> f64 {
    pick(quality, cap, TYPICAL_MPX, ms_per_mpx)
}

/// Sharp frames a second the server draws (what Settings shows).
pub(crate) fn fps(ms_per_mpx: f64) -> f64 {
    (1000.0 / (ms_per_mpx * PROBE_W * PROBE_H / 1e6)).min(60.0)
}

/// [`fps`] the other way round (`[browser] speed`).
pub(crate) fn ms_per_mpx(fps: f64) -> f64 {
    1000.0 / (fps.clamp(1.0, 60.0) * PROBE_W * PROBE_H / 1e6)
}

fn fast_cpu() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

// ---- virtual screens ----------------------------------------------------

/// The virtual screens of the running browser.
#[derive(Default)]
pub(crate) struct Screens {
    /// Tabs have windows on screens of their own scale.
    pub on: bool,
    /// Scales (×100) whose screen exists.
    made: Vec<u16>,
    windows: std::collections::HashMap<i64, f64>,
    placement: Arc<tokio::sync::Mutex<()>>,
}

impl Screens {
    pub fn new(on: bool) -> Self {
        Self {
            on,
            ..Default::default()
        }
    }
}

/// Where the screen of scale `s` starts (pixels; the primary one is 1x at
/// 0). Far apart, so a window's position never falls on another screen
/// whichever way it is converted.
fn left_of(s: f64) -> i64 {
    const ORDER: [f64; 9] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 0.75, 0.5];
    let i = ORDER.iter().position(|x| (x - s).abs() < 1e-6).unwrap_or(0);
    if i == 0 {
        0
    } else {
        40_000 * 4i64.pow(i as u32 - 1)
    }
}

/// Where windows sit on their screen, and how wide screens are (pixels):
/// room for a 1280×800 window at 3x.
const INSET: i64 = 64;
const SCREEN_W: i64 = 6000;
const _: () = assert!((INSET + 1280) * 3 < SCREEN_W && (INSET + 800) * 3 < 4000);

/// Whether this browser has virtual screens (headless, recent versions).
pub(crate) async fn screens_supported(cdp: &Cdp) -> Result<bool, String> {
    match cdp
        .optional_call("Emulation.getScreenInfos", json!({}))
        .await?
    {
        None => Ok(false),
        Some(v) if v["screenInfos"].is_array() => Ok(true),
        Some(_) => Err("the browser returned invalid screen information".into()),
    }
}

impl Browser {
    /// Whether tabs get windows on screens of their own scale.
    pub(crate) fn screens_on(&self) -> bool {
        self.0.screens.lock().on
    }

    /// Make the screen of scale `s` if it isn't there yet.
    async fn ensure_screen(&self, cdp: &Cdp, s: f64) -> Result<(), String> {
        let key = (s * 100.0).round() as u16;
        if (s - 1.0).abs() < 1e-6 || self.0.screens.lock().made.contains(&key) {
            return Ok(());
        }
        cdp.call(
            None,
            "Emulation.addScreen",
            json!({
                "left": left_of(s),
                "top": 0,
                "width": SCREEN_W,
                "height": 4000,
                "devicePixelRatio": s,
                "label": format!("tessera-{key}"),
            }),
        )
        .await?;
        let mut sc = self.0.screens.lock();
        if !sc.made.contains(&key) {
            sc.made.push(key);
        }
        Ok(())
    }

    pub(crate) fn window_scale(&self, window: Option<i64>, fallback: f64) -> f64 {
        window
            .and_then(|w| self.0.screens.lock().windows.get(&w).copied())
            .unwrap_or(fallback)
    }

    /// Move window `window` (now on the screen of scale `from`) to the
    /// screen of scale `to`. Positions are taken in the window's current
    /// screen's own units.
    pub(crate) async fn place_window(
        &self,
        cdp: &Cdp,
        window: i64,
        from: f64,
        to: f64,
    ) -> Result<(), String> {
        let gate = self.0.screens.lock().placement.clone();
        let _guard = gate.lock().await;
        let from = self.window_scale(Some(window), from);
        if !from.is_finite() || from <= 0.0 {
            return Err("unknown window render scale".into());
        }
        self.ensure_screen(cdp, to).await?;
        let at = |p: i64| ((p + INSET) as f64 / from).round() as i64;
        cdp.call(
            None,
            "Browser.setWindowBounds",
            json!({
                "windowId": window,
                "bounds": {"left": at(left_of(to)), "top": at(0)},
            }),
        )
        .await?;
        self.0.screens.lock().windows.insert(window, to);
        Ok(())
    }

    /// The server draws slowly (measured).
    pub(crate) fn slow(&self) -> bool {
        self.0.speed.lock().is_some_and(|k| k > SLOW_MS_PER_MPX)
    }

    /// The render scale for a tab showing `need` (see [`pick`]).
    pub(crate) fn render_scale_for(&self, need: f64, css_mpx: f64) -> f64 {
        let quality = self
            .0
            .host()
            .map(|h| h.browser_config().quality)
            .unwrap_or_default();
        pick(quality, need, css_mpx, *self.0.speed.lock())
    }

    /// The scale every tab renders at when tabs can't have their own (or
    /// the most a tab may use, in a windowed browser).
    pub(crate) fn global_scale(&self) -> f64 {
        f64::from_bits(self.0.scale.load(std::sync::atomic::Ordering::Relaxed))
    }
}

// ---- measuring the server's speed ----------------------------------------

/// The measured speed, remembered per browser and CPU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Measured {
    pub key: String,
    /// Milliseconds to draw a megapixel of a moving page.
    pub ms_per_mpx: Option<f64>,
    /// The browser has no virtual screens (an older version).
    #[serde(default)]
    pub no_screens: bool,
}

pub(crate) fn speed_file(profile: &Path) -> PathBuf {
    profile.join("tessera-speed.json")
}

pub(crate) fn load(path: &Path) -> Option<Measured> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub(crate) fn save(path: &Path, m: &Measured) {
    if let Ok(b) = serde_json::to_vec(m) {
        let _ = std::fs::write(path, b);
    }
}

/// What the measurement depends on: the browser (its file, which changes
/// with every update) and the CPU.
pub(crate) fn key(bin: &Path) -> String {
    let real = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    let modified = std::fs::metadata(&real)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
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
    format!("{}|{modified}|{cpu}|{cores}", real.display())
}

/// The page the speed is measured on: text and pictures, moving every
/// frame.
const PROBE_PAGE: &str = "<!doctype html><meta charset=utf-8><style>body{margin:0;font:16px/1.5 sans-serif;color:#222;background:#fff}p{margin:10px 18px}i{display:block;height:120px;margin:10px 18px;border-radius:8px;background:linear-gradient(120deg,#3a7bd5,#f5af19)}#m{position:fixed;left:0;top:0;width:220px;height:28px;background:#0a66c2}</style><div id=m></div><script>for(let i=0;i<40;i++)document.write((i%5==2?'<i></i>':'')+'<p>'+'The server draws this page while it moves, a frame at a time, to see how fast it can. '.repeat(3)+'</p>');const m=document.getElementById('m');const f=t=>{m.style.transform='translate('+(t/3%2300)+'px,'+(t/7%1500)+'px)';requestAnimationFrame(f)};requestAnimationFrame(f)</script>";

impl Browser {
    fn measurement_now(&self) -> Instant {
        #[cfg(test)]
        if let Some(at) = self.0.measurement_times.lock().pop_front() {
            return at;
        }
        Instant::now()
    }

    /// Time how fast the browser draws a moving page: milliseconds per
    /// megapixel (the median time between frames of a 4-megapixel page,
    /// each taken as it comes). `scale`: the main screen's pixel ratio.
    pub(crate) async fn measure_speed(&self, cdp: &Arc<Cdp>, scale: f64) -> Option<f64> {
        let _admission = self.admit_page(None).await.ok()?;
        let target = cdp
            .call(
                None,
                "Target.createTarget",
                json!({"url": "about:blank", "background": true, "newWindow": true,
                       "left": INSET, "top": INSET, "width": 400, "height": 300}),
            )
            .await
            .ok()?["targetId"]
            .as_str()?
            .to_string();
        let r = self.time_frames(cdp, &target, scale).await;
        cdp.send(None, "Target.closeTarget", json!({"targetId": target}));
        match &r {
            Some(k) => tracing::info!(
                "browser speed: {:.1} ms per megapixel ({:.0} sharp frames a second)",
                k,
                fps(*k)
            ),
            None => tracing::warn!("couldn't measure how fast the browser draws"),
        }
        r
    }

    async fn time_frames(&self, cdp: &Arc<Cdp>, target: &str, scale: f64) -> Option<f64> {
        let session = self.claim_attached(target).await.ok()?;
        let mut rx = cdp.subscribe(&session);
        let s = Some(session.as_str());
        let _ = cdp
            .call(s, "Runtime.runIfWaitingForDebugger", json!({}))
            .await;
        cdp.call(s, "Page.enable", json!({})).await.ok()?;
        cdp.call(
            s,
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled": true}),
        )
        .await
        .ok()?;
        let (w, h) = ((PROBE_W / scale).round(), (PROBE_H / scale).round());
        cdp.call(
            s,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": w as u32, "height": h as u32, "deviceScaleFactor": 1, "mobile": false}),
        )
        .await
        .ok()?;
        let url = format!(
            "data:text/html;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, PROBE_PAGE)
        );
        cdp.call(s, "Page.navigate", json!({"url": url}))
            .await
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(4);
        // Loaded (or close enough).
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline.into(), rx.recv()).await {
            if ev.method == "Page.loadEventFired" {
                break;
            }
        }
        cdp.call(
            s,
            "Page.startScreencast",
            json!({"format": "jpeg", "quality": 60, "maxWidth": PROBE_W, "maxHeight": PROBE_H}),
        )
        .await
        .ok()?;
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut at = Vec::new();
        while at.len() < 18 {
            let Ok(Some(ev)) = tokio::time::timeout_at(deadline.into(), rx.recv()).await else {
                break;
            };
            if ev.method != "Page.screencastFrame" {
                continue;
            }
            cdp.send(
                s,
                "Page.screencastFrameAck",
                json!({"sessionId": ev.params["sessionId"]}),
            );
            at.push(self.measurement_now());
        }
        cdp.send(s, "Page.stopScreencast", json!({}));
        cdp.unsubscribe(&session);
        // The first few come while the page settles.
        let mut gaps: Vec<f64> = at
            .windows(2)
            .skip(3)
            .map(|p| p[1].duration_since(p[0]).as_secs_f64() * 1000.0)
            .collect();
        if gaps.len() < 3 {
            return None;
        }
        gaps.sort_by(|a, b| a.total_cmp(b));
        let median = gaps[gaps.len() / 2];
        Some(median / (PROBE_W * PROBE_H / 1e6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_steps() {
        assert_eq!(quantize(0.4, 1.0), 1.0);
        assert_eq!(quantize(0.4, 0.5), 0.5);
        assert_eq!(quantize(0.9, 0.75), 0.75);
        assert_eq!(quantize(1.0, 0.5), 1.0);
        assert_eq!(quantize(1.6, 1.0), 1.5);
        assert_eq!(quantize(2.625, 1.0), 2.5);
        assert_eq!(quantize(9.0, 1.0), 3.0);
        assert_eq!(step_up(0.6), 1.0);
        assert_eq!(step_up(1.6), 1.75);
    }

    #[test]
    fn auto_draws_as_sharp_as_the_server_keeps_smooth() {
        let phone = 412.0 * 915.0 / 1e6;
        let desk = 1280.0 * 800.0 / 1e6;
        // A fast server: every device at its own sharpness (within 2).
        let fast = Some(3.4);
        assert_eq!(pick(BrowserQuality::Auto, 2.0, phone, fast), 2.0);
        assert_eq!(pick(BrowserQuality::Auto, 1.0, desk, fast), 1.0);
        assert_eq!(pick(BrowserQuality::Auto, 2.0, desk, fast), 2.0);
        // A virtual CPU without AVX2 (15 sharp frames a second): desktops
        // at 1x, phones a little less sharp than they could be.
        let slow = Some(16.3);
        assert_eq!(pick(BrowserQuality::Auto, 2.0, desk, slow), 1.0);
        assert_eq!(pick(BrowserQuality::Auto, 2.0, phone, slow), 1.75);
        // Slower still (the user's server, 11): a desktop tile moves at
        // 0.75x (sharp once it stops), a phone at 1.5x.
        let slower = Some(22.0);
        assert_eq!(pick(BrowserQuality::Auto, 1.0, desk, slower), 0.75);
        assert_eq!(pick(BrowserQuality::Auto, 2.0, phone, slower), 1.5);
        // Sharp doesn't look at the speed; Smooth goes lower still there.
        assert_eq!(pick(BrowserQuality::Sharp, 2.0, desk, slow), 2.0);
        assert_eq!(pick(BrowserQuality::Smooth, 2.0, phone, fast), 1.0);
        assert_eq!(pick(BrowserQuality::Smooth, 1.0, desk, fast), 1.0);
        assert_eq!(pick(BrowserQuality::Smooth, 1.0, desk, slower), 0.5);
        assert_eq!(pick(BrowserQuality::Smooth, 2.0, phone, slower), 1.0);
        assert_eq!(pick(BrowserQuality::Auto, f64::NAN, phone, fast), 1.0);
        // Between steps: the next one up (frames are scaled down, not up).
        assert_eq!(pick(BrowserQuality::Sharp, 1.6, desk, slow), 1.75);
        assert_eq!(pick(BrowserQuality::Auto, 1.6, desk, Some(5.0)), 1.75);
        assert_eq!(pick(BrowserQuality::Auto, 1.6, desk, Some(8.4)), 1.5);
        // One scale for all: sized for a desktop tile.
        assert_eq!(global(BrowserQuality::Auto, 2.0, slow), 1.0);
        assert_eq!(global(BrowserQuality::Auto, 2.0, fast), 2.0);
        assert!((fps(16.3) - 15.0).abs() < 0.1, "{}", fps(16.3));
        assert_eq!(fps(1.0), 60.0);
        assert!((fps(ms_per_mpx(12.0)) - 12.0).abs() < 1e-9);
        assert!(ms_per_mpx(31.0) < SLOW_MS_PER_MPX && ms_per_mpx(29.0) > SLOW_MS_PER_MPX);
    }

    #[test]
    fn screens_never_overlap() {
        // In its own units a screen spans up to SCREEN_W / 0.5, and a
        // position converted either way (×3 at most) stays off the others.
        let mut lefts: Vec<i64> = STEPS.iter().map(|s| left_of(*s)).collect();
        lefts.sort();
        for w in lefts.windows(2) {
            assert!(w[1] > (w[0] + SCREEN_W * 2) * 3, "{w:?}");
        }
        assert!(lefts[8] * 2 < i32::MAX as i64);
    }
}
