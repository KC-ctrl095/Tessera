//! How the web view feels over a real connection: a device (end-to-end
//! encrypted, through a simulated network: delay, bandwidth, queue) watches
//! a real Chromium tab and draws its frames the way the app does (decode,
//! newest first, acknowledge). Reports frames per second, bytes per frame,
//! how old frames are when drawn, how far the picture lags behind a scroll
//! or a key press, what the picture looks like once things stop moving, and
//! the CPU the server and the browser use.
//!
//! Ignored (it takes about a minute and needs Chromium):
//!   cargo test -p tessera-server --test browser_perf -- --ignored --nocapture
//! `BENCH=lan,phone` picks scenarios; `BENCH_CLIENT=old` acts like an app
//! that takes one frame at a time and only draws JPEG; `BENCH_DIR` keeps
//! the server's data there (default: a temporary directory); `BENCH_LOG=1`
//! (or `trace`) prints every frame drawn and the server's decisions;
//! `BENCH_BROWSER` adds lines to `[browser]` (e.g. `quality = "sharp"`, or
//! `virtual_screens = false` and `quality = "sharp"` for one 2x scale for
//! every tab, as before per-tab scales; `\n` separates lines). Use
//! `--release` for server CPU figures that mean something.
//! `BENCH_HOST=machine` runs the pages in a machine's browser instead ("Web
//! views here"): a node in this process (its own runtime, `tsr-node`
//! threads) on the device's own pairing, reaching the server over a LAN
//! link, so frames go browser → node → server → device.
//! `BENCH_URL` scrolls that page instead of the article (a real site);
//! `BENCH_SAVE=<dir>` keeps a picture drawn mid-scroll and the one on
//! screen at rest there, to look at.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

// ---- the page ---------------------------------------------------------------

/// A long article. A bar of 2 × 24 black/white cells in the top left corner
/// codes, row 1: when the page last painted it (ms, low 24 bits); row 2: the
/// keys it has seen (high 8 bits) and its scroll position (low 16 bits).
/// `?clock` repaints it every animation frame (with a moving bar), so the
/// page never stops changing.
fn article() -> String {
    let words = "the a server browser frame tile phone tunnel latency pixel quality scroll page render picture network sharp text image video window budget motion still decode canvas device user agent model tool".split(' ').collect::<Vec<_>>();
    let mut seed = 7u64;
    let mut rnd = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let mut body = String::new();
    for i in 0..60 {
        if i % 6 == 0 {
            body += &format!(
                "<h2>Section {}: {} {}</h2>",
                i / 6 + 1,
                words[rnd(words.len())],
                words[rnd(words.len())]
            );
        }
        if i % 9 == 4 {
            body += &format!(
                "<div class=pic style=\"background:linear-gradient({}deg,hsl({},70%,55%),hsl({},60%,35%))\">Figure {i}</div>",
                rnd(360),
                rnd(360),
                rnd(360)
            );
        }
        let n = 50 + rnd(60);
        let text: Vec<&str> = (0..n).map(|_| words[rnd(words.len())]).collect();
        body += &format!(
            "<p>{}. <a href='#'>A link</a> and <b>bold words</b> in a line.</p>",
            text.join(" ")
        );
    }
    format!(
        r#"<!doctype html><html><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>Article</title>
<style>
body{{font:16px/1.55 system-ui,sans-serif;margin:0;padding:56px 18px 40px;color:#1d1d1f;background:#fff;max-width:760px}}
h2{{font-size:21px;margin-top:28px}} a{{color:#0a66c2}}
.pic{{height:180px;border-radius:10px;display:flex;align-items:end;padding:10px;color:#fff;font-weight:600}}
#bar{{position:fixed;left:0;top:0;display:grid;grid-template-columns:repeat(24,12px);grid-template-rows:12px 12px;gap:2px;padding:1px;background:#888;z-index:9}}
#bar i{{display:block;background:#fff}} #bar i.k{{background:#000}}
#spin{{position:fixed;right:12px;top:6px;width:120px;height:16px;background:#eee;border-radius:8px;overflow:hidden;z-index:9}}
#spin b{{display:block;height:100%;width:30px;background:#0a66c2;border-radius:8px}}
</style></head><body><div id=bar></div><div id=spin><b></b></div><h1>A long article</h1>{body}
<script>
const bar = document.getElementById('bar'), cells = [];
for (let i = 0; i < 48; i++) {{ const c = document.createElement('i'); bar.appendChild(c); cells.push(c); }}
let keys = 0; const clock = new URLSearchParams(location.search).has('clock');
const put = (row, v) => {{ for (let b = 0; b < 24; b++) cells[row * 24 + b].className = (v >> (23 - b)) & 1 ? 'k' : ''; }};
const paint = () => {{ put(0, Date.now() & 0xffffff); put(1, ((keys & 0xff) << 16) | (Math.round(scrollY) & 0xffff)); }};
addEventListener('keydown', () => {{ keys++; if (!clock) paint(); }}, true);
addEventListener('scroll', () => {{ if (!clock) paint(); }}, {{ passive: true }});
const spin = document.querySelector('#spin b');
const loop = (t) => {{ paint(); spin.style.transform = `translateX(${{(t / 8) % 120 - 30}}px)`; requestAnimationFrame(loop); }};
if (clock) requestAnimationFrame(loop); else paint();
</script></body></html>"#
    )
}

async fn site() -> String {
    let page = Arc::new(article());
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(move || {
            let page = page.clone();
            async move { axum::response::Html(page.to_string()) }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/")
}

// ---- the network ------------------------------------------------------------

#[derive(Clone, Copy)]
struct Link {
    /// Round trip (half each way).
    rtt: Duration,
    /// Bytes per second, server → device and device → server.
    down: f64,
    up: f64,
}

/// A TCP proxy that behaves like a slow link: bytes queue at the
/// bottleneck, go out at its rate, then take half the round trip to arrive.
/// The queue holds at most 250 ms of data (then the sender is held back).
async fn proxy(upstream: std::net::SocketAddr, link: Link) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let Ok(server) = TcpStream::connect(upstream).await else {
                continue;
            };
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            tokio::spawn(pipe(cr, sw, link.up, link.rtt / 2));
            tokio::spawn(pipe(sr, cw, link.down, link.rtt / 2));
        }
    });
    format!("http://{addr}")
}

async fn pipe(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    rate: f64,
    delay: Duration,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((at, data)) = rx.recv().await {
            tokio::time::sleep_until(at.into()).await;
            if to.write_all(&data).await.is_err() {
                break;
            }
        }
    });
    let queue = Duration::from_millis(250);
    let mut busy_until = Instant::now();
    let mut buf = vec![0u8; 8192];
    loop {
        if busy_until > Instant::now() + queue {
            tokio::time::sleep_until((busy_until - queue).into()).await;
        }
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let start = busy_until.max(Instant::now());
        busy_until = start + Duration::from_secs_f64(n as f64 / rate);
        if tx.send((busy_until + delay, buf[..n].to_vec())).is_err() {
            break;
        }
    }
}

// ---- measuring --------------------------------------------------------------

/// CPU seconds of this process's threads whose name starts with `prefix`.
fn threads_cpu(prefix: &str) -> f64 {
    let tck = 100.0;
    let mut total = 0.0;
    for e in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let p = e.path();
        let name = std::fs::read_to_string(p.join("comm")).unwrap_or_default();
        if !name.starts_with(prefix) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(p.join("stat")) else {
            continue;
        };
        let f: Vec<&str> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        total += (f[11].parse::<f64>().unwrap_or(0.0) + f[12].parse::<f64>().unwrap_or(0.0)) / tck;
    }
    total
}

/// CPU seconds of every process in group `pgid` (the browser).
fn group_cpu(pgid: u32) -> f64 {
    let mut cpu = 0.0;
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.get(2).and_then(|g| g.parse::<u32>().ok()) == Some(pgid) {
            cpu +=
                (f[11].parse::<f64>().unwrap_or(0.0) + f[12].parse::<f64>().unwrap_or(0.0)) / 100.0;
        }
    }
    cpu
}

/// CPU seconds of the browser's threads by process type and thread name
/// (e.g. `gpu-process/VizCompositorTh`).
fn group_threads(pgid: u32) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let ticks = |stat: &str| -> Option<(u32, f64)> {
        let f: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let t = (f[11].parse::<f64>().ok()? + f[12].parse::<f64>().ok()?) / 100.0;
        Some((f.get(2)?.parse().ok()?, t))
    };
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Some((group, _)) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| ticks(&s))
        else {
            continue;
        };
        if group != pgid {
            continue;
        }
        let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        // Chromium rewrites its children's titles: spaces between arguments.
        let kind = cmd
            .split(['\0', ' '])
            .find_map(|a| a.strip_prefix("--type="))
            .unwrap_or("browser")
            .to_string();
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for t in tasks.flatten() {
            let name = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
            let name = name.trim().trim_end_matches(|c: char| c.is_ascii_digit());
            if let Some((_, cpu)) = std::fs::read_to_string(t.path().join("stat"))
                .ok()
                .and_then(|s| ticks(&s))
            {
                *out.entry(format!("{kind}/{name}")).or_insert(0.0) += cpu;
            }
        }
    }
    out
}

/// The busiest browser threads between two samples (% of a core).
fn busiest(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>, secs: f64) -> String {
    let mut d: Vec<(String, f64)> = b
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                (v - a.get(k).copied().unwrap_or(0.0)) / secs * 100.0,
            )
        })
        .filter(|(_, v)| *v >= 5.0)
        .collect();
    d.sort_by(|x, y| y.1.total_cmp(&x.1));
    d.iter()
        .take(4)
        .map(|(k, v)| format!("{k} {v:.0}%"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The browser a machine runs for web views (its profile folder): the pid
/// of its main process.
fn machine_browser_pid(profile: &std::path::Path) -> Option<u32> {
    let needle = format!("--user-data-dir={}", profile.display());
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let cmd = String::from_utf8_lossy(&cmd);
        if cmd.split('\0').any(|a| a == needle) && !cmd.contains("--type=") {
            return Some(pid);
        }
    }
    None
}

/// A machine with "Web views here" on, in `dir`, on `login`: running
/// until the returned token is cancelled.
fn start_node(
    rt: &tokio::runtime::Runtime,
    dir: &std::path::Path,
    login: ServerConfig,
) -> tokio_util::sync::CancellationToken {
    use tessera_core::node::{LinkEvent, Node, NodeConfig};
    let paths = Paths::at(dir);
    paths.ensure().unwrap();
    let mut c = NodeConfig::load(&paths).unwrap();
    c.enabled = true;
    c.name = "Desktop".into();
    c.generation = 1;
    c.scope.webviews = true;
    c.save(&paths).unwrap();
    struct ClientLink(Client);
    impl tessera_core::node::Link for ClientLink {
        fn call(
            &self,
            req: Request,
        ) -> futures::future::BoxFuture<'static, Result<serde_json::Value, ApiError>> {
            let c = self.0.clone();
            Box::pin(async move { c.call_timeout(req, Duration::from_secs(30)).await })
        }
    }
    let stop = tokio_util::sync::CancellationToken::new();
    let s2 = stop.clone();
    rt.spawn(async move {
        let client = Client::connect(login);
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let mut sub = client.subscribe();
        let c2 = client.clone();
        tokio::spawn(async move {
            let mut up = false;
            if c2.status().state == tessera_client::ConnState::Connected {
                up = true;
                let _ = tx.send(LinkEvent::Up(None));
            }
            while let Ok(ev) = sub.recv().await {
                let _ = match ev {
                    ClientEvent::Connected => {
                        up = true;
                        tx.send(LinkEvent::Up(None))
                    }
                    ClientEvent::Status(st)
                        if st.state != tessera_client::ConnState::Connected && up =>
                    {
                        up = false;
                        tx.send(LinkEvent::Down(st.error))
                    }
                    ClientEvent::Event(e) => tx.send(LinkEvent::Event(e)),
                    _ => Ok(()),
                };
            }
        });
        let node = Node::open(paths).unwrap();
        node.serve(Arc::new(ClientLink(client.clone())), events, s2)
            .await;
        client.close();
    });
    stop
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Width, height and (JPEG) quality of a picture.
fn picture_info(mime: &str, b: &[u8]) -> (u32, u32, Option<u32>) {
    if mime == "image/webp" && b.len() > 30 {
        return match &b[12..16] {
            b"VP8X" => (
                1 + u32::from_le_bytes([b[24], b[25], b[26], 0]),
                1 + u32::from_le_bytes([b[27], b[28], b[29], 0]),
                None,
            ),
            b"VP8 " => (
                u16::from_le_bytes([b[26], b[27]]) as u32 & 0x3fff,
                u16::from_le_bytes([b[28], b[29]]) as u32 & 0x3fff,
                None,
            ),
            _ => (0, 0, None),
        };
    }
    // JPEG: the frame header for the size, the first quantisation table for
    // the quality (libjpeg's scaling of the standard luminance table).
    const STD: [u32; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    let (mut w, mut h, mut q) = (0, 0, None);
    let mut i = 2;
    while i + 9 < b.len() && b[i] == 0xFF {
        let m = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if m == 0xDB && q.is_none() && i + 4 + 65 <= b.len() {
            let t = &b[i + 5..i + 5 + 64];
            let sum: u32 = t.iter().map(|v| *v as u32).sum();
            let s = sum as f64 * 100.0 / STD.iter().sum::<u32>() as f64;
            q = Some(
                if s <= 100.0 {
                    (200.0 - s) / 2.0
                } else {
                    5000.0 / s
                }
                .round() as u32,
            );
        }
        if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            break;
        }
        i += 2 + len;
    }
    (w, h, q)
}

/// The page's bar, read from a decoded frame: (paint time, keys, scroll).
fn read_bar(rgb: &[u8], w: u32, css_w: u32) -> Option<(u32, u32, u32)> {
    let k = w as f64 / css_w.max(1) as f64;
    let mut rows = [0u32; 2];
    for (r, row) in rows.iter_mut().enumerate() {
        for c in 0..24 {
            let x = ((1 + c * 14 + 6) as f64 * k) as usize;
            let y = ((1 + r * 14 + 6) as f64 * k) as usize;
            let i = (y * w as usize + x) * 3;
            let px = rgb.get(i..i + 3)?;
            let lum = (px[0] as u32 + px[1] as u32 + px[2] as u32) / 3;
            *row = (*row << 1) | (lum < 128) as u32;
        }
    }
    Some((rows[0], rows[1] >> 16, rows[1] & 0xffff))
}

#[derive(Default, Clone, Debug)]
struct Drawn {
    frame: u64,
    /// When it was drawn.
    at: Option<Instant>,
    at_ms: u64,
    bytes: usize,
    mime: String,
    px: (u32, u32),
    quality: Option<u32>,
    /// What the bar says (JPEG frames only).
    bar: Option<(u32, u32, u32)>,
    /// The picture itself (with `BENCH_SAVE`).
    data: Option<Arc<Vec<u8>>>,
}

/// What the device drew (shared with the measuring code).
#[derive(Default)]
struct Screen {
    drawn: Vec<Drawn>,
    received: u64,
    received_bytes: u64,
}

/// A frame waiting to be drawn: (number, mime, base64 data, CSS width).
type Pending = Arc<Mutex<Option<(u64, String, String, u32)>>>;

/// Draw frames as the app does: decode the newest, skip older ones (each
/// acknowledged once drawn or skipped).
fn watch(client: Client, board: String, screen: Arc<Mutex<Screen>>) -> tokio::task::JoinHandle<()> {
    let mut events = client.subscribe();
    tokio::spawn(async move {
        let latest: Pending = Arc::default();
        let wake = Arc::new(tokio::sync::Notify::new());
        let drawer = {
            let (latest, wake, client, board, screen) = (
                latest.clone(),
                wake.clone(),
                client.clone(),
                board.clone(),
                screen.clone(),
            );
            tokio::spawn(async move {
                loop {
                    wake.notified().await;
                    let Some((n, mime, data, css_w)) = latest.lock().take() else {
                        continue;
                    };
                    let d = tokio::task::spawn_blocking(move || {
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(&data)
                            .unwrap_or_default();
                        let (w, h, quality) = picture_info(&mime, &bytes);
                        let bar = (mime == "image/jpeg")
                            .then(|| {
                                let mut dec =
                                    jpeg_decoder::Decoder::new(std::io::Cursor::new(&bytes));
                                let rgb = dec.decode().ok()?;
                                read_bar(&rgb, w, css_w)
                            })
                            .flatten();
                        let data = std::env::var_os("BENCH_SAVE").map(|_| Arc::new(bytes.clone()));
                        Drawn {
                            frame: n,
                            at: Some(Instant::now()),
                            at_ms: now_ms(),
                            bytes: bytes.len(),
                            mime,
                            px: (w, h),
                            quality,
                            bar,
                            data,
                        }
                    })
                    .await
                    .unwrap();
                    screen.lock().drawn.push(d);
                    let c = client.clone();
                    let b = board.clone();
                    tokio::spawn(async move {
                        c.call(Request::BrowserAck {
                            board_id: b,
                            frame: n,
                        })
                        .await
                    });
                }
            })
        };
        while let Ok(ev) = events.recv().await {
            if let ClientEvent::Event(Event::BrowserFrame {
                board_id,
                frame,
                mime,
                data,
                width,
                ..
            }) = ev
            {
                if board_id != board {
                    continue;
                }
                {
                    let mut s = screen.lock();
                    s.received += 1;
                    s.received_bytes += (data.len() * 3 / 4) as u64;
                }
                let skipped = latest.lock().replace((frame, mime, data, width));
                if let Some((old, ..)) = skipped {
                    let c = client.clone();
                    let b = board.clone();
                    tokio::spawn(async move {
                        c.call(Request::BrowserAck {
                            board_id: b,
                            frame: old,
                        })
                        .await
                    });
                }
                wake.notify_one();
            }
        }
        drawer.abort();
    })
}

/// With `BENCH_LOG`: every frame drawn since `t0`, and the inputs sent.
fn dump(label: &str, t0: Instant, drawn: &[Drawn], inputs: &[(Instant, String)]) {
    if std::env::var("BENCH_LOG").is_err() {
        return;
    }
    let mut rows: Vec<(Instant, String)> =
        inputs.iter().filter(|(t, _)| *t >= t0).cloned().collect();
    for d in drawn {
        let Some(at) = d.at.filter(|a| *a >= t0) else {
            continue;
        };
        rows.push((
            at,
            format!(
                "#{} {} {}×{} {} {:.0} KB{}",
                d.frame,
                d.mime.trim_start_matches("image/"),
                d.px.0,
                d.px.1,
                d.quality.map(|q| format!("q{q}")).unwrap_or_default(),
                d.bytes as f64 / 1024.0,
                d.bar
                    .map(|b| format!(" keys {} y {}", b.1, b.2))
                    .unwrap_or_default()
            ),
        ));
    }
    rows.sort_by_key(|r| r.0);
    eprintln!("-- {label}");
    for (t, r) in rows {
        eprintln!("  {:>6.0} ms  {r}", (t - t0).as_secs_f64() * 1000.0);
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

struct Scenario {
    name: &'static str,
    link: Link,
    width: u32,
    height: u32,
    dpr: f64,
    mobile: bool,
}

#[test]
#[ignore]
fn web_view_performance() {
    if !tessera_core::browser::installed(None) {
        eprintln!("no Chromium-family browser installed; skipping");
        return;
    }
    if let Ok(level) = std::env::var("BENCH_LOG") {
        let level = if level == "trace" { "trace" } else { "debug" };
        let _ = tracing_subscriber::fmt()
            .with_env_filter(format!("tessera_core::browser={level}"))
            .with_writer(std::io::stderr)
            .try_init();
    }
    let old_client = std::env::var("BENCH_CLIENT").is_ok_and(|v| v == "old");
    let only = std::env::var("BENCH").ok();
    let on_machine = std::env::var("BENCH_HOST").is_ok_and(|v| v == "machine");
    let mbit = |m: f64| m * 1e6 / 8.0;
    let scenarios = [
        Scenario {
            name: "lan",
            link: Link {
                rtt: Duration::from_millis(2),
                down: mbit(300.0),
                up: mbit(300.0),
            },
            width: 1280,
            height: 800,
            dpr: 1.0,
            mobile: false,
        },
        Scenario {
            name: "phone",
            link: Link {
                rtt: Duration::from_millis(100),
                down: mbit(20.0),
                up: mbit(10.0),
            },
            width: 412,
            height: 915,
            dpr: 2.625,
            mobile: true,
        },
        Scenario {
            name: "fold",
            link: Link {
                rtt: Duration::from_millis(100),
                down: mbit(20.0),
                up: mbit(10.0),
            },
            width: 840,
            height: 900,
            dpr: 2.625,
            mobile: true,
        },
        Scenario {
            name: "phone-slow",
            link: Link {
                rtt: Duration::from_millis(150),
                down: mbit(8.0),
                up: mbit(4.0),
            },
            width: 412,
            height: 915,
            dpr: 2.625,
            mobile: true,
        },
    ];
    // `BENCH_LINK=rtt_ms/down_mbit/up_mbit` and `BENCH_DPR` override every
    // scenario's network and pixel ratio.
    let mut scenarios = scenarios;
    for sc in scenarios.iter_mut() {
        if let Some(v) = std::env::var("BENCH_LINK").ok().map(|v| {
            v.split('/')
                .map(|x| x.parse::<f64>().unwrap())
                .collect::<Vec<_>>()
        }) {
            sc.link = Link {
                rtt: Duration::from_millis(v[0] as u64),
                down: mbit(v[1]),
                up: mbit(v[2]),
            };
        }
        if let Some(d) = std::env::var("BENCH_DPR").ok().and_then(|d| d.parse().ok()) {
            sc.dpr = d;
        }
    }
    let srv = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("tsr-srv")
        .enable_all()
        .build()
        .unwrap();
    let cli = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("bench-cli")
        .enable_all()
        .build()
        .unwrap();
    let node_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tsr-node")
        .enable_all()
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::env::var("BENCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    std::fs::create_dir_all(&dir).unwrap();
    let browser_cfg = std::env::var("BENCH_BROWSER")
        .unwrap_or_default()
        .replace("\\n", "\n");
    std::fs::write(
        dir.join("config.toml"),
        format!("default_model = \"x/m\"\n[browser]\n{browser_cfg}\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n"),
    )
    .unwrap();
    let (engine, server, page) = srv.block_on(async {
        let engine = tessera_core::Engine::open(Paths::at(&dir), Default::default())
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = tessera_server::router(engine.clone(), None).unwrap();
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
        (engine, addr, site().await)
    });

    let mut report = Vec::new();
    for sc in scenarios.iter().filter(|s| {
        only.as_deref()
            .is_none_or(|o| o.split(',').any(|n| n == s.name))
    }) {
        let engine = engine.clone();
        let page = page.clone();
        let node_dir = dir.join(format!("node-{}", sc.name));
        let profile = tessera_core::node::browser::dir(&Paths::at(&node_dir)).join("profile");
        let pid = |e: &tessera_core::Engine| {
            if on_machine {
                machine_browser_pid(&profile)
            } else {
                srv.block_on(e.browser().pid())
            }
        };
        let r = cli.block_on(async {
            let url = proxy(server, sc.link).await;
            let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
            let (_, grant) =
                tessera_client::pair(std::slice::from_ref(&url), &BTreeMap::new(), &code, sc.name)
                    .await
                    .unwrap();
            let login = ServerConfig {
                endpoints: vec![url],
                headers: BTreeMap::new(),
                token: grant.token,
                server_key: grant.server_key,
            };
            let client = Client::connect(login.clone());
            client
                .wait_connected(Duration::from_secs(10))
                .await
                .unwrap();
            (client, login)
        });
        let (client, login) = r;
        // The device is also a machine with web views (its own pairing, as
        // the desktop app hands it to the node it turns on), on the same LAN
        // as the server.
        let node = on_machine.then(|| {
            let lan = Link {
                rtt: Duration::from_millis(2),
                down: 300e6 / 8.0,
                up: 300e6 / 8.0,
            };
            let node_login = ServerConfig {
                endpoints: vec![node_rt.block_on(proxy(server, lan))],
                ..login.clone()
            };
            start_node(&node_rt, &node_dir, node_login)
        });
        let r = cli.block_on(async {
            if node.is_some() {
                for _ in 0..200 {
                    let list: Vec<MachineInfo> =
                        client.call_as(Request::ListMachines).await.unwrap();
                    if list.iter().any(|m| m.online && m.scope.webviews) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            let session: SessionInfo = client
                .call_as(Request::CreateSession {
                    cwd: Some(std::env::temp_dir().display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                })
                .await
                .unwrap();
            let mut boards = Vec::new();
            let still_page = std::env::var("BENCH_URL").unwrap_or_else(|_| page.clone());
            for u in [format!("{page}?clock"), still_page] {
                let b: Whiteboard = client
                    .call_as(Request::CreateBoard {
                        session_id: session.id.clone(),
                        title: "Article".into(),
                        placed: false,
                        content: BoardContent::Url { url: u },
                    })
                    .await
                    .unwrap();
                assert_eq!(b.host.is_some(), on_machine, "where the page runs");
                boards.push(b.id);
            }
            boards
        });
        let boards = r;
        let attach = |board: &str| {
            let mut req = serde_json::json!({
                "method": "browser_attach",
                "params": {"board_id": board, "width": sc.width, "height": sc.height, "dpr": sc.dpr, "mobile": sc.mobile},
            });
            if !old_client {
                req["params"]["pipelined"] = true.into();
                req["params"]["formats"] = serde_json::json!(["image/webp"]);
            }
            let req: Request = serde_json::from_value(req).unwrap();
            cli.block_on(client.call(req)).unwrap();
        };
        let detach = |board: &str| {
            cli.block_on(client.call(Request::BrowserDetach {
                board_id: board.into(),
            }))
            .unwrap();
        };
        let input = |board: &str, event: BrowserInputEvent| {
            let c = client.clone();
            let b = board.to_string();
            cli.spawn(async move { c.call(Request::BrowserInput { board_id: b, event }).await });
        };
        let mut lines = vec![format!(
            "== {} ({}×{} @{}{}, rtt {} ms, {:.0}/{:.0} Mbit/s){}",
            sc.name,
            sc.width,
            sc.height,
            sc.dpr,
            if sc.mobile { " mobile" } else { "" },
            sc.link.rtt.as_millis(),
            sc.link.down * 8.0 / 1e6,
            sc.link.up * 8.0 / 1e6,
            if old_client { " [old client]" } else { "" },
        )];

        // 1. An animated page.
        let screen = Arc::new(Mutex::new(Screen::default()));
        let w = {
            let _g = cli.enter();
            watch(client.clone(), boards[0].clone(), screen.clone())
        };
        attach(&boards[0]);
        std::thread::sleep(Duration::from_secs(2));
        let chrome = pid(&engine).unwrap();
        let (c0, s0, t0) = (group_cpu(chrome), threads_cpu("tsr-srv"), Instant::now());
        let n0c = threads_cpu("tsr-node");
        let th0 = group_threads(chrome);
        let (n0, b0) = {
            let mut s = screen.lock();
            s.drawn.clear();
            (s.received, s.received_bytes)
        };
        std::thread::sleep(Duration::from_secs(5));
        let secs = t0.elapsed().as_secs_f64();
        let (c1, s1) = (group_cpu(chrome), threads_cpu("tsr-srv"));
        let n1c = threads_cpu("tsr-node");
        let th1 = group_threads(chrome);
        {
            let s = screen.lock();
            let n = s.drawn.len();
            let recv = s.received - n0;
            let bytes = s.received_bytes - b0;
            let mut ages: Vec<f64> = s
                .drawn
                .iter()
                .filter_map(|d| {
                    d.bar
                        .map(|b| ((d.at_ms as u32).wrapping_sub(b.0) & 0xffffff) as f64)
                })
                .filter(|a| *a < 60_000.0)
                .collect();
            let q = s
                .drawn
                .last()
                .map(|d| (d.px, d.quality))
                .unwrap_or_default();
            lines.push(format!(
                "animated: {:.1} fps drawn ({:.1} received), {:.0} KB/frame ({}×{} q{:?}), {:.0} KB/s; frame age when drawn: median {:.0} ms, p90 {:.0} ms; browser CPU {:.0}%, server CPU {:.1}%",
                n as f64 / secs,
                recv as f64 / secs,
                bytes as f64 / recv.max(1) as f64 / 1024.0,
                q.0.0,
                q.0.1,
                q.1.unwrap_or(0),
                bytes as f64 / secs / 1024.0,
                pct(&mut ages, 0.5),
                pct(&mut ages, 0.9),
                (c1 - c0) / secs * 100.0,
                (s1 - s0) / secs * 100.0,
            ));
            if on_machine {
                lines.push(format!("  node CPU {:.1}%", (n1c - n0c) / secs * 100.0));
            }
            lines.push(format!("  busiest: {}", busiest(&th0, &th1, secs)));
        }
        detach(&boards[0]);
        w.abort();

        // 2. A still page: key presses, then scrolling, then rest.
        let screen = Arc::new(Mutex::new(Screen::default()));
        let w = {
            let _g = cli.enter();
            watch(client.clone(), boards[1].clone(), screen.clone())
        };
        attach(&boards[1]);
        std::thread::sleep(Duration::from_secs(3));
        let mut lat = Vec::new();
        let mut sent: Vec<(Instant, String)> = Vec::new();
        let keys_t0 = Instant::now();
        for k in 0..8u32 {
            let keys_before = screen
                .lock()
                .drawn
                .iter()
                .rev()
                .find_map(|d| d.bar)
                .map(|b| b.1)
                .unwrap_or(0);
            let t = Instant::now();
            sent.push((t, format!("key {k}")));
            input(
                &boards[1],
                BrowserInputEvent::Key {
                    kind: BrowserKeyKind::Down,
                    key: "x".into(),
                    code: "KeyX".into(),
                    text: None,
                    modifiers: 0,
                },
            );
            input(
                &boards[1],
                BrowserInputEvent::Key {
                    kind: BrowserKeyKind::Up,
                    key: "x".into(),
                    code: "KeyX".into(),
                    text: None,
                    modifiers: 0,
                },
            );
            let deadline = t + Duration::from_secs(3);
            loop {
                let seen = screen
                    .lock()
                    .drawn
                    .iter()
                    .rev()
                    .find(|d| {
                        d.at.is_some_and(|a| a > t) && d.bar.is_some_and(|b| b.1 != keys_before)
                    })
                    .and_then(|d| d.at);
                if let Some(a) = seen {
                    lat.push((a - t).as_secs_f64() * 1000.0);
                    break;
                }
                if Instant::now() > deadline {
                    lat.push(f64::NAN);
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = k;
            std::thread::sleep(Duration::from_millis(500));
        }
        dump("key presses", keys_t0, &screen.lock().drawn, &sent);
        lat.retain(|v| v.is_finite());
        lines.push(format!(
            "key press → picture: median {:.0} ms, p90 {:.0} ms ({} of 8 seen)",
            pct(&mut lat, 0.5),
            pct(&mut lat, 0.9),
            lat.len()
        ));

        // Scrolling for 4 s: wheel turns (desktop) or finger drags (phone),
        // 60 a second.
        let intent: Arc<Mutex<Vec<(Instant, f64)>>> = Arc::default();
        {
            let mut s = screen.lock();
            s.drawn.clear();
        }
        let (n0, b0) = {
            let s = screen.lock();
            (s.received, s.received_bytes)
        };
        let (c0, s0) = (group_cpu(chrome), threads_cpu("tsr-srv"));
        let th0 = group_threads(chrome);
        let t0 = Instant::now();
        let step = 8.0;
        let mut total = 0.0;
        let mut tick = 0u32;
        let mut finger = 700.0;
        while t0.elapsed() < Duration::from_secs(4) {
            if sc.mobile {
                // Drags of 36 steps (~290 px) with a short pause between.
                let phase = tick % 45;
                let pt = |y: f64| vec![BrowserTouchPoint { x: 200.0, y, id: 1 }];
                match phase {
                    0 => {
                        finger = 700.0;
                        input(
                            &boards[1],
                            BrowserInputEvent::Touch {
                                kind: BrowserTouchKind::Start,
                                points: pt(finger),
                                modifiers: 0,
                            },
                        );
                    }
                    1..=36 => {
                        finger -= step;
                        total += step;
                        intent.lock().push((Instant::now(), total));
                        input(
                            &boards[1],
                            BrowserInputEvent::Touch {
                                kind: BrowserTouchKind::Move,
                                points: pt(finger),
                                modifiers: 0,
                            },
                        );
                    }
                    37 => input(
                        &boards[1],
                        BrowserInputEvent::Touch {
                            kind: BrowserTouchKind::End,
                            points: vec![],
                            modifiers: 0,
                        },
                    ),
                    _ => {}
                }
            } else {
                total += step;
                intent.lock().push((Instant::now(), total));
                input(
                    &boards[1],
                    BrowserInputEvent::Mouse {
                        kind: BrowserMouseKind::Wheel,
                        x: 400.0,
                        y: 400.0,
                        button: BrowserMouseButton::None,
                        clicks: 0,
                        dx: 0.0,
                        dy: step,
                        modifiers: 0,
                    },
                );
            }
            tick += 1;
            let next = t0 + Duration::from_secs_f64(tick as f64 / 60.0);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        if sc.mobile && tick > 0 && (tick - 1) % 45 <= 36 {
            // The finger lifts.
            input(
                &boards[1],
                BrowserInputEvent::Touch {
                    kind: BrowserTouchKind::End,
                    points: vec![],
                    modifiers: 0,
                },
            );
        }
        let secs = t0.elapsed().as_secs_f64();
        std::thread::sleep(Duration::from_millis(300));
        let (c1, s1) = (group_cpu(chrome), threads_cpu("tsr-srv"));
        let th1 = group_threads(chrome);
        {
            let s = screen.lock();
            let moving: Vec<&Drawn> = s
                .drawn
                .iter()
                .filter(|d| d.at.is_some_and(|a| a < t0 + Duration::from_secs_f64(secs)))
                .collect();
            // How old the picture is when drawn: the page's paint time is
            // in its bar (touch drags fling on after the finger lifts, so
            // the finger's position isn't a fair yardstick).
            let mut ages: Vec<f64> = moving
                .iter()
                .filter_map(|d| {
                    let b = d.bar?;
                    let age = ((d.at_ms as u32).wrapping_sub(b.0) & 0xffffff) as f64;
                    (age < 60_000.0).then_some(age)
                })
                .collect();
            let recv = s.received - n0;
            let bytes = s.received_bytes - b0;
            // How sharp moving frames were: their pixels per CSS pixel.
            let mut scales: Vec<f64> = moving
                .iter()
                .filter(|d| d.px.0 > 0)
                .map(|d| d.px.0 as f64 / sc.width as f64)
                .collect();
            let mut qs: Vec<f64> = moving
                .iter()
                .filter_map(|d| d.quality.map(f64::from))
                .collect();
            lines.push(format!(
                "  moving frames: {:.2}x median (p10 {:.2}x), JPEG q{:.0} median",
                pct(&mut scales, 0.5),
                pct(&mut scales, 0.1),
                pct(&mut qs, 0.5)
            ));
            lines.push(format!(
                "scrolling: {:.1} fps drawn ({:.1} received), {:.0} KB/frame, {:.0} KB/s; frame age when drawn: median {:.0} ms, p90 {:.0} ms; browser CPU {:.0}%, server CPU {:.1}%",
                moving.len() as f64 / secs,
                recv as f64 / secs,
                bytes as f64 / recv.max(1) as f64 / 1024.0,
                bytes as f64 / secs / 1024.0,
                pct(&mut ages, 0.5),
                pct(&mut ages, 0.9),
                (c1 - c0) / secs * 100.0,
                (s1 - s0) / secs * 100.0,
            ));
            lines.push(format!("  busiest: {}", busiest(&th0, &th1, secs)));
        }
        // At rest: what is on screen a moment later.
        std::thread::sleep(Duration::from_millis(1700));
        {
            let s = screen.lock();
            let marks: Vec<(Instant, String)> = intent
                .lock()
                .iter()
                .step_by(6)
                .map(|(t, v)| (*t, format!("input at {v}")))
                .collect();
            dump("scrolling and rest", t0, &s.drawn, &marks);
            let last = s.drawn.last().cloned().unwrap_or_default();
            let end = t0 + Duration::from_secs_f64(secs);
            if let Ok(dir) = std::env::var("BENCH_SAVE") {
                let ext = |d: &Drawn| d.mime.trim_start_matches("image/").replace("jpeg", "jpg");
                let mid = t0 + Duration::from_secs_f64(secs / 2.0);
                let moving = s.drawn.iter().find(|d| d.at.is_some_and(|a| a >= mid));
                for (what, d) in [("moving", moving), ("rest", Some(&last))] {
                    if let Some(d) = d.filter(|d| d.data.is_some()) {
                        let f = format!("{dir}/{}-{what}.{}", sc.name, ext(d));
                        std::fs::write(&f, d.data.as_deref().unwrap()).unwrap();
                        lines.push(format!("  saved {f} ({}×{})", d.px.0, d.px.1));
                    }
                }
            }
            let after: Vec<&Drawn> = s
                .drawn
                .iter()
                .filter(|d| d.at.is_some_and(|a| a > end))
                .collect();
            // Still pictures: WebP, or JPEG from a slow server, at the
            // device's pixel ratio (sharper than any live frame on a
            // phone; on a 1x screen they can't be told apart).
            let live_px = s
                .drawn
                .iter()
                .filter(|d| d.at.is_some_and(|a| a <= end))
                .map(|d| d.px.0)
                .max()
                .unwrap_or(0);
            let still = after
                .iter()
                .find(|d| d.mime == "image/webp" || d.px.0 > live_px)
                .and_then(|d| d.at)
                .map(|a| {
                    format!(
                        ", sharp {:.0} ms after the scroll ended",
                        (a - end).as_secs_f64() * 1000.0
                    )
                })
                .unwrap_or_default();
            lines.push(format!(
                "at rest: {} × {} {} {}, {:.0} KB ({} frames after the scroll ended{still})",
                last.px.0,
                last.px.1,
                last.mime,
                last.quality.map(|q| format!("q{q}")).unwrap_or_default(),
                last.bytes as f64 / 1024.0,
                after.len(),
            ));
        }
        detach(&boards[1]);
        w.abort();
        if let Some(n) = node {
            n.cancel();
            std::thread::sleep(Duration::from_millis(500));
        }
        for l in &lines {
            println!("{l}");
        }
        report.extend(lines);
        client.close();
    }
    let info: BrowserInfo = srv
        .block_on(engine.handle(Request::GetBrowser, None))
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap();
    if on_machine {
        report.push("pages ran in a machine's browser (node in this process, LAN to the server); node CPU is in `tsr-node`".into());
    }
    report.push(format!(
        "browser: {} quality {:?}, measured {} sharp frames a second{}",
        info.name.unwrap_or_default(),
        info.quality,
        info.speed.map_or("?".into(), |s| format!("{s:.0}")),
        if browser_cfg.is_empty() {
            String::new()
        } else {
            format!(" [{}]", browser_cfg.replace('\n', "; "))
        }
    ));
    println!("\n{}", report.join("\n"));
    srv.block_on(engine.browser().shutdown());
}
