//! HTTP/WebSocket front end. See `docs/PROTOCOL.md`.
//!
//! Designed to sit behind anything: plain LAN, a reverse proxy, or a
//! Cloudflare tunnel with Access service tokens (custom headers pass through
//! untouched; long-lived sockets are kept alive with pings).
//!
//! Apps talk to the server end-to-end encrypted (`/api/pair/*`,
//! `/api/secure`; see `tessera-secure`), so a proxy that terminates TLS only
//! relays ciphertext. The plain token socket (`/api/ws`) exists for the
//! browser UI and is refused from the network unless
//! `security.require_e2ee = false`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tessera_core::Engine;
use tessera_core::config::Paths;
use tessera_core::store::Login;
use tessera_proto::e2e::{
    PairFinish, PairFinished, PairGrant, PairHello, PairStart, PairStarted, SessionAuth,
    SessionAuthReply,
};
use tessera_proto::{
    ApiError, ClientMessage, ErrorCode, Event, PairRequest, PairResponse, Request, ServerMessage,
};
use tessera_secure::pairing::PairKeys;
use tessera_secure::{Channel, Identity, Opener, Sealer};
use tokio::sync::{Semaphore, broadcast, mpsc};

mod connection_guard;
use connection_guard::{Authority, watch_cut, watch_revocations, while_live};
use tokio_util::sync::CancellationToken;

mod voice;
mod web;
mod web_access;

pub struct ServerOptions {
    pub bind: SocketAddr,
    /// Directory with a built web UI to serve at `/` (optional).
    pub web_dir: Option<PathBuf>,
}

struct AppState {
    engine: Engine,
    web_dir: Option<PathBuf>,
    /// The web UI's compressed files (see `web.rs`).
    web: web::WebFiles,
    identity: Identity,
    pairing: Mutex<Pairing>,
    /// Encrypted sockets still in their (unauthenticated) handshake.
    handshakes: Arc<Semaphore>,
    /// The same, by the address they come from.
    handshakes_by_ip: Arc<PerIp>,
}

/// Unauthenticated handshakes in progress at once; each may take 15 s, so
/// this bounds what a stranger can tie up.
const MAX_HANDSHAKES: usize = 64;
/// …and from one address, so one stranger can't take them all.
const MAX_HANDSHAKES_PER_IP: usize = 8;
/// The device's first handshake message must arrive within this.
const FIRST_MESSAGE: Duration = Duration::from_secs(5);
/// Connections open at once, and from one address on the network (a tunnel
/// or proxy on this computer carries many users, so loopback counts only
/// toward the total). Sockets that became WebSockets don't count: they
/// passed the handshake or the token check.
const MAX_CONNECTIONS: usize = 512;
const MAX_CONNECTIONS_PER_IP: usize = 64;
/// A request's headers must arrive within this (a kept-alive connection
/// waiting for its next request closes after it too).
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Total time to receive a request body, not an idle timeout reset by each byte.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// JSON requests were already limited to 2 MiB by axum; apply it to every route.
const MAX_REQUEST_BODY: usize = 2 << 20;

/// Things in progress by the address they come from.
#[derive(Default)]
struct PerIp(Mutex<HashMap<IpAddr, usize>>);

/// One of them, given back when dropped.
struct IpSlot {
    map: Arc<PerIp>,
    ip: IpAddr,
}

impl PerIp {
    fn take(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<IpSlot> {
        let mut m = self.0.lock();
        let n = m.entry(ip).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(IpSlot {
            map: self.clone(),
            ip,
        })
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut m = self.map.0.lock();
        if let Some(n) = m.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.ip);
            }
        }
    }
}

/// `::ffff:1.2.3.4` is `1.2.3.4`.
fn plain_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    plain_ip(ip).is_loopback()
}

/// Where a request really comes from: the peer, or for a tunnel or proxy
/// on this computer (a loopback peer) the client it names.
fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let ip = plain_ip(peer.ip());
    if !ip.is_loopback() {
        return ip;
    }
    let named = |name: &str, last: bool| {
        let v = headers.get(name)?.to_str().ok()?;
        let part = if last { v.rsplit(',').next() } else { Some(v) };
        part?.trim().parse::<IpAddr>().ok().map(plain_ip)
    };
    named("cf-connecting-ip", false)
        .or_else(|| named("x-forwarded-for", true))
        .unwrap_or(ip)
}

/// Where the server's long-term key lives (owner-only, never shown to tools).
pub fn identity_file(paths: &Paths) -> PathBuf {
    paths.data_dir.join("identity.key")
}

pub fn load_identity(paths: &Paths) -> Result<Identity> {
    Identity::load_or_create(&identity_file(paths))
}

/// How long a device has between the two pairing steps.
const PAIRING_TTL: Duration = Duration::from_secs(120);
/// Wrong answers allowed for one code before it is cancelled.
const CODE_ATTEMPTS: u32 = 3;

/// Brakes on pairing, kept apart for this computer (a browser or app on the
/// server itself), the network and web browsers ("Use Tessera in a web
/// browser", which strangers can reach), so strangers can't lock the owner
/// out at the server: 10 failures and 60 attempts per 10 minutes each, a few
/// wrong answers per code, and a small number of attempts in flight. The
/// web door counts by address instead (5 failures and 10 attempts each; 1
/// and 2 while the door as a whole is past its limits): an address there
/// is whatever a proxy on this computer says, which any program on it can
/// make up, so its limits only ever slow everyone down, never shut the
/// door for the owner.
struct Pairing {
    local: Budget,
    network: Budget,
    web: Budget,
    /// The web door's tries by address.
    web_by_ip: HashMap<IpAddr, Budget>,
    /// Answers tried per code (counted before they are checked).
    code_tries: HashMap<String, u32>,
    pending: HashMap<String, PendingPair>,
}

/// The window the budgets count in.
const PAIRING_WINDOW: Duration = Duration::from_secs(600);
/// Pairing attempts in flight at once, separately for local apps, network
/// apps and the web door. A stranger cannot occupy the owner's local pool.
const MAX_PAIRING_IN_FLIGHT: usize = 16;

struct Budget {
    window_start: Instant,
    failures: u32,
    starts: u32,
}

impl Budget {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            failures: 0,
            starts: 0,
        }
    }

    fn allowed(&mut self) -> bool {
        self.within(10, u32::MAX)
    }

    /// Fewer than `failures` failures and `starts` attempts in this window.
    fn within(&mut self, failures: u32, starts: u32) -> bool {
        if self.window_start.elapsed() > PAIRING_WINDOW {
            *self = Self::new();
        }
        self.failures < failures && self.starts < starts
    }
}

/// Which way a pairing comes in (each has its own brakes).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Door {
    /// The server's own computer.
    Local,
    /// The apps, from the network.
    Network,
    /// A web browser at `/web/api/pair/*`, from this address.
    Web(IpAddr),
}

struct PendingPair {
    /// `None`: no code was active. Answered all the same, so nobody learns
    /// whether one is.
    code: Option<String>,
    keys: PairKeys,
    created: Instant,
    door: Door,
}

impl Pairing {
    fn new() -> Self {
        Self {
            local: Budget::new(),
            network: Budget::new(),
            web: Budget::new(),
            web_by_ip: HashMap::new(),
            code_tries: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    fn budget(&mut self, door: Door) -> &mut Budget {
        self.pending
            .retain(|_, p| p.created.elapsed() < PAIRING_TTL);
        match door {
            Door::Local => &mut self.local,
            Door::Network => &mut self.network,
            Door::Web(_) => &mut self.web,
        }
    }

    /// The web door's budget for the address it comes from.
    fn ip_budget(&mut self, door: Door) -> Option<&mut Budget> {
        let Door::Web(ip) = door else {
            return None;
        };
        if self.web_by_ip.len() > 4096 {
            self.web_by_ip
                .retain(|_, b| b.window_start.elapsed() <= PAIRING_WINDOW);
            // Made-up addresses can be had without end: past this many, all
            // start afresh. Nothing is lost by it, as each code takes only
            // a few wrong answers wherever they come from.
            if self.web_by_ip.len() > 16_384 {
                self.web_by_ip.clear();
            }
        }
        Some(self.web_by_ip.entry(ip).or_insert_with(Budget::new))
    }

    /// Another attempt may start through `door` (counted if so).
    fn start(&mut self, door: Door) -> bool {
        let busy = !self.budget(door).within(10, 60);
        let ok = match self.ip_budget(door) {
            Some(b) if busy => b.within(1, 2),
            Some(b) => b.within(5, 10),
            None => !busy,
        };
        if ok {
            self.budget(door).starts += 1;
            if let Some(b) = self.ip_budget(door) {
                b.starts += 1;
            }
        }
        ok
    }

    /// May still try through `door` (not too many failures).
    fn allowed(&mut self, door: Door) -> bool {
        let busy = !self.budget(door).allowed();
        match self.ip_budget(door) {
            Some(b) => b.within(if busy { 1 } else { 5 }, u32::MAX),
            None => !busy,
        }
    }

    fn failed(&mut self, door: Door) {
        self.budget(door).failures += 1;
        if let Some(b) = self.ip_budget(door) {
            b.failures += 1;
        }
    }

    /// Count one answer for `code` before it is checked, so answers sent
    /// all at once can't get past the limit. False once it is used up.
    fn try_code(&mut self, code: &str) -> bool {
        let tries = self.code_tries.entry(code.to_string()).or_default();
        *tries += 1;
        *tries <= CODE_ATTEMPTS
    }
}

pub fn router(engine: Engine, web_dir: Option<PathBuf>) -> Result<Router> {
    let identity = load_identity(engine.paths()).context("loading the server identity key")?;
    // "Use Tessera in a web browser" can be switched on only with a web app
    // to give (`--web`, or the one built in).
    engine.set_web_app(web_dir.is_some() || web::has_built());
    let state = Arc::new(AppState {
        engine,
        web_dir,
        web: Default::default(),
        identity,
        pairing: Mutex::new(Pairing::new()),
        handshakes: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
        handshakes_by_ip: Arc::default(),
    });
    Ok(Router::new()
        .route("/api/health", get(health))
        .route("/api/pair", post(pair))
        .route("/api/pair/start", post(pair_start))
        .route("/api/pair/finish", post(pair_finish))
        .route("/api/ws", get(ws_upgrade))
        .route("/api/secure", get(secure_upgrade))
        .route("/board/{id}/{token}/", get(board_page))
        .route("/preview/{token}/", get(preview_page))
        .route("/board-lib/{*path}", get(board_lib))
        .nest("/web", web_access::router(state.clone()))
        .fallback(get(static_files))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web_access::top,
        ))
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state))
}

/// On every response: no MIME sniffing, no framing (clickjacking the
/// approval buttons), no Referer leaking URLs to other sites.
async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if !h.contains_key(header::X_FRAME_OPTIONS) {
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

/// Content Security Policy for the web UI. Images are local only, so model
/// output can't carry conversation data off in an image URL; inline scripts
/// run only if they are the page's own (by hash).
fn csp(html: &str) -> String {
    let hashes = inline_script_hashes(html);
    format!(
        "default-src 'self'; script-src 'self'{hashes}; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self' http: https: ws: wss:; frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// ` 'sha256-…'` for each inline script of `html`: those (the page's own)
/// may run, nothing else inline.
fn inline_script_hashes(html: &str) -> String {
    use sha2::Digest as _;
    let mut hashes = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("<script") {
        rest = &rest[i + 7..];
        let Some(open_end) = rest.find('>') else {
            break;
        };
        let attrs = &rest[..open_end];
        rest = &rest[open_end + 1..];
        let Some(close) = rest.find("</script>") else {
            break;
        };
        if !attrs.contains("src=") {
            let digest = sha2::Sha256::digest(&rest.as_bytes()[..close]);
            hashes.push_str(&format!(" 'sha256-{}'", tessera_secure::b64(&digest)));
        }
        rest = &rest[close..];
    }
    hashes
}

#[derive(serde::Deserialize)]
struct BoardQuery {
    #[serde(default)]
    theme: Option<String>,
}

/// A whiteboard's page for the browser UI on this machine. The page token
/// comes from an authenticated `board_link` request; the page itself gets a
/// policy with no network access, and the UI frames it sandboxed.
async fn board_page(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::extract::Path((id, token)): axum::extract::Path<(String, String)>,
    Query(q): Query<BoardQuery>,
) -> Response {
    if !direct_local(peer, &headers) {
        return e2ee_required();
    }
    let Some(html) = st.engine.board_page(&id, &token) else {
        return error(StatusCode::NOT_FOUND, "no such whiteboard");
    };
    framed_page(&html, &headers, q.theme.as_deref() != Some("light"))
}

/// A preview of a note's HTML (`preview_page`): served like a whiteboard's
/// page, while its token is good.
async fn preview_page(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::extract::Path(token): axum::extract::Path<String>,
    Query(q): Query<BoardQuery>,
) -> Response {
    if !direct_local(peer, &headers) {
        return e2ee_required();
    }
    let Some(html) = st.engine.preview_page(&token) else {
        return error(StatusCode::NOT_FOUND, "this preview has expired");
    };
    framed_page(&html, &headers, q.theme.as_deref() != Some("light"))
}

/// A board or preview page: the policy with no network (also in the page),
/// framed only by this origin, never cached.
fn framed_page(html: &str, headers: &HeaderMap, dark: bool) -> Response {
    // Only reached directly on this machine, so over plain http.
    let origin = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| format!("http://{h}"));
    let origin = origin.as_deref();
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                tessera_proto::board::csp(origin),
            ),
            (header::X_FRAME_OPTIONS, "SAMEORIGIN".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        tessera_proto::board::document(html, dark, origin),
    )
        .into_response()
}

/// The libraries boards may use (public code). Boards run on an opaque
/// origin, so module imports need CORS.
async fn board_lib(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    match tessera_proto::board::lib(&path) {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            bytes,
        )
            .into_response(),
        None => error(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Details go to the log, not to whoever made the request.
fn internal_error(e: impl std::fmt::Display) -> Response {
    tracing::error!("{e}");
    error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
}

/// Serve `router` on an already-bound listener (peer addresses are needed to
/// tell local browsers from proxied ones).
pub async fn serve_on(listener: tokio::net::TcpListener, router: Router) -> std::io::Result<()> {
    serve_until(listener, router, stop_signal()).await
}

/// [`serve_on`] until `stop` resolves (then connections wind down). Like
/// `axum::serve`, with brakes for strangers: headers must arrive in time,
/// and connections are capped in total and per address.
async fn serve_until(
    listener: tokio::net::TcpListener,
    router: Router,
    stop: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    use tower::ServiceExt as _;
    // Run before extraction on every route, including public pairing.
    // Only a local proxy may name an effective client address.
    let requests_by_ip = Arc::new(PerIp::default());
    let router = router.layer(axum::middleware::from_fn(move |req, next| {
        request_body(req, next, requests_by_ip.clone())
    }));
    let total = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let per_ip = Arc::new(PerIp::default());
    // Told to wind down; each connection holds a `done` until it closes.
    let (wind_down, _) = tokio::sync::watch::channel(false);
    let (done_tx, done_rx) = tokio::sync::watch::channel(());
    let mut stop = std::pin::pin!(stop);
    loop {
        let permit = tokio::select! {
            p = total.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => break,
            },
            () = &mut stop => break,
        };
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(v) => v,
                Err(e) => {
                    // Out of file descriptors and the like: wait a moment.
                    tracing::debug!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut stop => break,
        };
        let slot = if is_loopback(peer.ip()) {
            None
        } else {
            match per_ip.take(plain_ip(peer.ip()), MAX_CONNECTIONS_PER_IP) {
                Some(s) => Some(s),
                None => continue,
            }
        };
        // Small messages (replies, input, the tail of a web view frame) go
        // out at once instead of waiting for the previous one's
        // acknowledgement.
        let _ = tcp.set_nodelay(true);
        let service = router.clone().map_request(
            move |mut req: axum::extract::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                req.map(axum::body::Body::new)
            },
        );
        let service = hyper_util::service::TowerToHyperService::new(service);
        let mut wind = wind_down.subscribe();
        let done = done_rx.clone();
        tokio::spawn(async move {
            let _held = (permit, slot, done);
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(hyper_util::rt::TokioIo::new(tcp), service)
                .with_upgrades();
            let mut conn = std::pin::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => {}
                _ = wind.changed() => {
                    conn.as_mut().graceful_shutdown();
                    let _ = conn.await;
                }
            }
        });
    }
    drop(listener);
    let _ = wind_down.send(true);
    drop(done_rx);
    done_tx.closed().await;
    Ok(())
}

/// Admit before receiving bytes. HTTP/1 connections with an incomplete or
/// refused body must close, so neither their slot nor unread bytes survive.
async fn request_body(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    by_ip: Arc<PerIp>,
) -> Response {
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let _slot = if direct_local(peer, req.headers()) {
        None
    } else {
        match by_ip.take(client_ip(peer, req.headers()), MAX_CONNECTIONS_PER_IP) {
            Some(slot) => Some(slot),
            None => return close_request(StatusCode::TOO_MANY_REQUESTS),
        }
    };
    let (parts, body) = req.into_parts();
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_REQUEST_BODY as u64)
    {
        return close_request(StatusCode::PAYLOAD_TOO_LARGE);
    }
    match tokio::time::timeout(
        BODY_READ_TIMEOUT,
        axum::body::to_bytes(body, MAX_REQUEST_BODY),
    )
    .await
    {
        Ok(Ok(bytes)) => {
            next.run(axum::extract::Request::from_parts(
                parts,
                axum::body::Body::from(bytes),
            ))
            .await
        }
        Ok(Err(_)) => close_request(StatusCode::PAYLOAD_TOO_LARGE),
        Err(_) => close_request(StatusCode::REQUEST_TIMEOUT),
    }
}

fn close_request(status: StatusCode) -> Response {
    (status, [(header::CONNECTION, "close")]).into_response()
}

/// Ctrl+C, or (Unix) the SIGTERM `kill` and service managers send.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(windows)]
    tessera_core::platform::windows::stop_signal().await;
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn serve(engine: Engine, opts: ServerOptions) -> Result<()> {
    let router = router(engine.clone(), opts.web_dir)?;
    let listener = tokio::net::TcpListener::bind(opts.bind)
        .await
        .with_context(|| format!("binding {}", opts.bind))?;
    tracing::info!("listening on http://{}", opts.bind);
    // What is working is noted the moment the stop is asked for, before
    // connections wind down: a turn cut off on the way is continued later.
    let stopping = engine.clone();
    let served = serve_until(listener, router, async move {
        stop_signal().await;
        stopping.begin_shutdown();
    })
    .await;
    // The browser and cloudflared are separate processes: stop them too.
    engine.shutdown().await;
    served?;
    Ok(())
}

/// True for a browser or client on this machine talking to us directly: a
/// loopback peer with no sign of a proxy in between. A tunnel daemon or
/// reverse proxy on the same host also connects from loopback, but adds
/// forwarding headers (cloudflared always does) or carries the public host
/// name, so it does not count.
fn direct_local(peer: SocketAddr, headers: &HeaderMap) -> bool {
    const PROXY_HEADERS: [&str; 8] = [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "cf-connecting-ip",
        "cf-ray",
        "via",
    ];
    if !is_loopback(peer.ip()) || PROXY_HEADERS.iter().any(|h| headers.contains_key(*h)) {
        return false;
    }
    let text = |name: header::HeaderName| {
        headers
            .get(name)
            .map(|v| v.to_str().unwrap_or("\u{0}").to_string())
    };
    let host_ok = text(header::HOST).is_none_or(|h| loopback_host(&h));
    let origin_ok = text(header::ORIGIN).is_none_or(|o| {
        o.split_once("://")
            .is_some_and(|(_, rest)| loopback_host(rest.split('/').next().unwrap_or("")))
    });
    host_ok && origin_ok
}

/// `localhost:7878`, `127.0.0.1`, `[::1]:7878`…
fn loopback_host(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Plain (not end-to-end encrypted) access: always for this machine, and for
/// the network only when the owner turned `require_e2ee` off.
fn plain_allowed(st: &AppState, peer: SocketAddr, headers: &HeaderMap) -> bool {
    !st.engine.config().security.require_e2ee || direct_local(peer, headers)
}

fn e2ee_required() -> Response {
    error(
        StatusCode::FORBIDDEN,
        "This server only accepts end-to-end encrypted connections from the network. Use the Tessera app, or open this page on the server itself (http://localhost). To allow plain browser access, set `require_e2ee = false` under [security] in config.toml.",
    )
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": {"message": message}}))).into_response()
}

async fn health(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Health is public and read-only; allow cross-origin probing.
    // Public and unauthenticated: say only what a client needs to find us,
    // and whether this request counts as made on this computer (`tessera
    // remote` checks that a proxy in front of the server says who connects).
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(
            json!({ "name": "tessera", "version": env!("CARGO_PKG_VERSION"), "protocol": tessera_proto::PROTOCOL_VERSION, "e2ee": true, "local": direct_local(peer, &headers) }),
        ),
    )
}

fn device_name(raw: &str) -> String {
    let name = tessera_core::util::one_line(raw, 60);
    if name.is_empty() {
        "device".to_string()
    } else {
        name
    }
}

async fn server_info(st: &AppState) -> Option<tessera_proto::ServerInfo> {
    let v = st
        .engine
        .handle(
            Request::Hello {
                client: "pair".into(),
                version: String::new(),
            },
            None,
        )
        .await
        .ok()?;
    serde_json::from_value(v).ok()
}

/// Plain pairing for the browser UI (the token is visible to anything that
/// can read the traffic, so this is local-only by default).
async fn pair(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PairRequest>,
) -> Response {
    if !plain_allowed(&st, peer, &headers) {
        return e2ee_required();
    }
    let door = if direct_local(peer, &headers) {
        Door::Local
    } else {
        Door::Network
    };
    if !st.pairing.lock().allowed(door) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many pairing attempts; try again in a few minutes",
        );
    }
    match st.engine.store().consume_pairing_code(&req.code) {
        Ok(true) => {}
        Ok(false) => {
            st.pairing.lock().failed(door);
            return error(StatusCode::UNAUTHORIZED, "invalid or expired pairing code");
        }
        Err(e) => return internal_error(e),
    }
    let name = device_name(&req.device_name);
    match st.engine.store().add_device(&name) {
        Ok((device_id, token)) => {
            let Some(server) = server_info(&st).await else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            tracing::info!("paired new device `{name}` ({device_id}, browser)");
            Json(PairResponse {
                token,
                device_id,
                server,
            })
            .into_response()
        }
        Err(e) => internal_error(e),
    }
}

/// Encrypted pairing, step 1: answer the device's SPAKE2 message for the
/// active code. Nothing here reveals whether the device used the right code,
/// or whether a code is active at all.
async fn pair_start(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PairStart>,
) -> Response {
    let door = if direct_local(peer, &headers) {
        Door::Local
    } else {
        Door::Network
    };
    start_pairing(&st, req, door)
}

/// [`pair_start`] through `door`.
fn start_pairing(st: &AppState, req: PairStart, door: Door) -> Response {
    let Ok(msg) = tessera_secure::unb64(&req.pake) else {
        return error(StatusCode::BAD_REQUEST, "invalid pairing message");
    };
    if msg.len() > 128 {
        return error(StatusCode::BAD_REQUEST, "invalid pairing message");
    }
    let mut p = st.pairing.lock();
    if !p.start(door) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many pairing attempts; try again in a few minutes",
        );
    }
    // Local apps, network apps and the web door each have their own pool.
    // On the web door the oldest gives way to a new attempt: whoever keeps
    // it full can't keep the owner out.
    let web = matches!(door, Door::Web(_));
    let in_flight: Vec<(&String, Instant)> = p
        .pending
        .iter()
        .filter(|(_, pp)| {
            matches!(
                (pp.door, door),
                (Door::Local, Door::Local)
                    | (Door::Network, Door::Network)
                    | (Door::Web(_), Door::Web(_))
            )
        })
        .map(|(id, pp)| (id, pp.created))
        .collect();
    if in_flight.len() >= MAX_PAIRING_IN_FLIGHT {
        if !web {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many pairings in progress; try again in a minute",
            );
        }
        if let Some(oldest) = in_flight
            .iter()
            .min_by_key(|(_, created)| *created)
            .map(|(id, _)| (*id).clone())
        {
            p.pending.remove(&oldest);
        }
    }
    let code = match st.engine.store().active_pairing_code() {
        Ok(c) => c,
        Err(e) => return internal_error(e),
    };
    // No code active: answer for one nobody knows.
    let answer_for = code.clone().unwrap_or_else(|| {
        tessera_core::util::random_token(16)
            .bytes()
            .map(|b| char::from(b'0' + b % 10))
            .take(8)
            .collect()
    });
    let Ok((keys, reply)) = tessera_secure::pairing::respond(&answer_for, &msg) else {
        return error(StatusCode::BAD_REQUEST, "invalid pairing message");
    };
    let id = tessera_core::util::random_token(24);
    p.pending.insert(
        id.clone(),
        PendingPair {
            code,
            keys,
            created: Instant::now(),
            door,
        },
    );
    Json(PairStarted {
        pairing: id,
        pake: tessera_secure::b64(&reply),
    })
    .into_response()
}

/// Encrypted pairing, step 2: the device proves it used the same code by
/// sealing its hello under the shared key; the server answers with the device
/// token and its own public key, sealed the same way.
async fn pair_finish(State(st): State<Arc<AppState>>, Json(req): Json<PairFinish>) -> Response {
    finish_pairing(&st, req, false).await
}

/// [`pair_finish`] for an app (`web` false) or a web browser: an attempt
/// finishes the way it started.
async fn finish_pairing(st: &AppState, req: PairFinish, web: bool) -> Response {
    let (pending, within) = {
        let mut p = st.pairing.lock();
        let pending = p
            .pending
            .remove(&req.pairing)
            .filter(|pp| pp.created.elapsed() < PAIRING_TTL)
            .filter(|pp| matches!(pp.door, Door::Web(_)) == web);
        let within = match pending.as_ref().and_then(|pp| pp.code.as_deref()) {
            Some(code) => p.try_code(code),
            None => true,
        };
        (pending, within)
    };
    let Some(pending) = pending else {
        return error(
            StatusCode::UNAUTHORIZED,
            "this pairing attempt expired; try again",
        );
    };
    // Past the code's tries: not even checked.
    let hello = tessera_secure::unb64(&req.sealed)
        .ok()
        .filter(|_| within)
        .and_then(|sealed| pending.keys.open_from_device(&sealed).ok());
    let (Some(hello), Some(code)) = (hello, &pending.code) else {
        let mut p = st.pairing.lock();
        p.failed(pending.door);
        if let Some(code) = &pending.code
            && p.code_tries.get(code).is_some_and(|n| *n >= CODE_ATTEMPTS)
        {
            p.code_tries.remove(code);
            let _ = st.engine.store().consume_pairing_code(code);
            tracing::warn!("pairing code cancelled after {CODE_ATTEMPTS} wrong attempts");
            return error(
                StatusCode::UNAUTHORIZED,
                "wrong pairing code; that code is now cancelled, so create a new one",
            );
        }
        return error(StatusCode::UNAUTHORIZED, "wrong pairing code");
    };
    let Ok(hello) = serde_json::from_slice::<PairHello>(&hello) else {
        return error(StatusCode::BAD_REQUEST, "invalid pairing message");
    };
    // A browser brings its own key (and is never a machine).
    let browser_key = match (web, &hello.device_key) {
        (false, _) => None,
        (true, Some(k)) if !hello.machine => tessera_secure::unb64(k)
            .ok()
            .filter(|k| tessera_secure::browser_key_ok(k)),
        (true, _) => None,
    };
    if web && browser_key.is_none() {
        return error(
            StatusCode::BAD_REQUEST,
            "this browser can't keep a key of its own; use an up-to-date browser",
        );
    }
    // Single use: of two devices racing with the same code, one wins.
    match st.engine.store().consume_pairing_code(code) {
        Ok(true) => {}
        Ok(false) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "that pairing code expired or was already used",
            );
        }
        Err(e) => return internal_error(e),
    }
    st.pairing.lock().code_tries.remove(code);
    let name = device_name(&hello.device_name);
    let added = match &browser_key {
        Some(key) => st.engine.store().add_browser(&name, key),
        None => st.engine.store().add_device_as(&name, hello.machine),
    };
    let (device_id, token) = match added {
        Ok(v) => v,
        Err(e) => return internal_error(e),
    };
    let Some(server) = server_info(st).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    tracing::info!(
        "paired new {} `{name}` ({device_id}, end-to-end encrypted)",
        if hello.machine {
            "machine"
        } else if web {
            "web browser"
        } else {
            "device"
        }
    );
    let grant = PairGrant {
        token,
        device_id,
        server_key: tessera_secure::b64(st.identity.public()),
        server,
        machine: hello.machine,
    };
    let Ok(plain) = serde_json::to_vec(&grant) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let sealed = pending.keys.seal_to_device(&plain);
    Json(PairFinished {
        sealed: tessera_secure::b64(&sealed),
    })
    .into_response()
}

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

async fn ws_upgrade(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if !plain_allowed(&st, peer, &headers) {
        return e2ee_required();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
        .or(q.token);
    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };
    // Listening for revocations before the token is checked: a device
    // revoked in between still finds its connection closed.
    let revocations = st.engine.revocations();
    let login = match st.engine.store().login(&token) {
        // A web browser's login works only with its key, at `/web`.
        Ok(Some(l)) if l.browser_key.is_none() => l,
        Ok(_) => return (StatusCode::UNAUTHORIZED, "invalid or revoked token").into_response(),
        Err(e) => return internal_error(e),
    };
    // A plain connection is private only when made on this computer itself
    // (terminals need that; see `engine/terminals.rs`).
    let private = direct_local(peer, &headers);
    ws.max_message_size(tessera_proto::MAX_SERIALIZED_REQUEST_BYTES)
        .on_upgrade(move |socket| {
            connection(
                st,
                socket,
                Arc::new(Authority { login, token }),
                revocations,
                None,
                private,
                false,
            )
        })
}

async fn secure_upgrade(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let busy = || {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many connections are being set up; try again shortly",
        )
    };
    let Some(slot) = st
        .handshakes_by_ip
        .take(client_ip(peer, &headers), MAX_HANDSHAKES_PER_IP)
    else {
        return busy();
    };
    let Ok(permit) = st.handshakes.clone().try_acquire_owned() else {
        return busy();
    };
    // From another computer (directly, or through a tunnel or proxy).
    let elsewhere = !direct_local(peer, &headers);
    // Every frame is one Noise message (≤ 64 KiB); big messages are chunked.
    ws.max_message_size(70 << 10)
        .max_frame_size(70 << 10)
        .on_upgrade(move |socket| secure_connection(st, socket, (permit, slot), false, elsewhere))
}

/// Next binary message, skipping keepalives.
async fn next_binary(socket: &mut WebSocket) -> Result<Vec<u8>> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(_)) | None => anyhow::bail!("connection closed during handshake"),
            Some(Err(e)) => return Err(e.into()),
        }
    }
}

async fn send_sealed(
    socket: &mut WebSocket,
    sealer: &mut Sealer,
    value: &impl serde::Serialize,
) -> Result<()> {
    for frame in sealer.seal(&serde_json::to_vec(value)?)? {
        socket.send(Message::Binary(frame.into())).await?;
    }
    Ok(())
}

/// Noise handshake, then the device token inside the encrypted channel.
/// `web`: a web browser's socket (`/web/api/secure`), where only browsers'
/// logins work, each with the proof that it holds its key; elsewhere their
/// logins don't work at all.
async fn secure_connection(
    st: Arc<AppState>,
    mut socket: WebSocket,
    handshake: (tokio::sync::OwnedSemaphorePermit, IpSlot),
    web: bool,
    elsewhere: bool,
) {
    let setup = async {
        let first = tokio::time::timeout(FIRST_MESSAGE, next_binary(&mut socket))
            .await
            .map_err(|_| anyhow::anyhow!("no handshake within {FIRST_MESSAGE:?}"))??;
        let (mut channel, reply) = match tessera_secure::accept(&st.identity, &first) {
            Ok(v) => v,
            Err(e) => {
                // Most likely the device pinned a different key (this server was
                // reset). Say so; it reveals nothing and the device can't
                // proceed either way.
                let frame = CloseFrame {
                    code: HANDSHAKE_FAILED,
                    reason: "handshake failed".into(),
                };
                let _ = socket.send(Message::Close(Some(frame))).await;
                return Err(e);
            }
        };
        socket.send(Message::Binary(reply.into())).await?;
        channel.opener.set_limit(16 << 10);
        let auth = loop {
            let frame = next_binary(&mut socket).await?;
            if let Some(m) = channel.opener.open(&frame)? {
                break m;
            }
        };
        let auth: SessionAuth = serde_json::from_slice(&auth)?;
        // Listening for revocations before the token is checked: a device
        // revoked in between still finds its connection closed.
        let revocations = st.engine.revocations();
        let proof = auth
            .proof
            .as_deref()
            .and_then(|p| tessera_secure::unb64(p).ok());
        // Switched off while this browser was connecting: said as it is, so
        // it keeps its pairing (and comes back when it's on again).
        let off = web && !st.engine.web_access_on();
        let login = st
            .engine
            .store()
            .login(&auth.token)?
            .filter(|l| match (&l.browser_key, web) {
                (None, false) => true,
                (Some(key), true) => {
                    !off && proof.as_deref().is_some_and(|p| {
                        tessera_secure::browser_proof_ok(
                            key,
                            tessera_proto::e2e::BROWSER_PROOF,
                            &channel.handshake_hash,
                            p,
                        )
                    })
                }
                _ => false,
            });
        match login {
            Some(login) => {
                send_sealed(
                    &mut socket,
                    &mut channel.sealer,
                    &SessionAuthReply {
                        ok: true,
                        error: None,
                    },
                )
                .await?;
                channel
                    .opener
                    .set_limit(tessera_proto::MAX_SERIALIZED_REQUEST_BYTES);
                Ok(Some((
                    channel,
                    Arc::new(Authority {
                        login,
                        token: auth.token,
                    }),
                    revocations,
                )))
            }
            None => {
                let error = ApiError::new(
                    ErrorCode::Unauthorized,
                    if off {
                        WEB_ACCESS_OFF
                    } else {
                        "this device's token was revoked or is unknown; pair again"
                    },
                );
                send_sealed(
                    &mut socket,
                    &mut channel.sealer,
                    &SessionAuthReply {
                        ok: false,
                        error: Some(error),
                    },
                )
                .await?;
                Ok::<_, anyhow::Error>(None)
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(15), setup).await;
    drop(handshake);
    match result {
        // End-to-end encrypted, but a web browser's page comes from whoever
        // serves the address (a tunnel's edge, a proxy) and could have been
        // changed on the way: never private (no terminals from browsers).
        Ok(Ok(Some((channel, authority, revocations)))) => {
            // The server's addresses and Access headers (`get_server_route`):
            // the user's apps, and a machine on another computer (it needs
            // them to stay reachable away from home); never a web browser.
            let route = !web && (!authority.login.machine || elsewhere);
            connection(
                st,
                socket,
                authority,
                revocations,
                Some(channel),
                !web,
                route,
            )
            .await
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::debug!("encrypted handshake failed: {e:#}"),
        Err(_) => tracing::debug!("encrypted handshake timed out"),
    }
}

/// A browser's login refused only because "Use Tessera in a web browser" is
/// off (the page keeps its pairing; `transport.ts` looks for these words).
const WEB_ACCESS_OFF: &str = "Web browser access is turned off on this server.";

/// WebSocket close code for a Noise handshake the server could not read.
pub const HANDSHAKE_FAILED: u16 = 4001;

/// Requests one connection may have in flight before we stop reading.
const MAX_IN_FLIGHT: usize = 64;

/// Bytes of events one connection may have waiting to be sent before it
/// misses events and is asked to resync instead (see `connection`).
const EVENT_BACKLOG: usize = 1 << 20;

/// Bytes of replies and frames waiting for one connection's socket before
/// it takes no more (its requests then wait).
const QUEUE_LIMIT: usize = 8 << 20;

/// A message on its way to a connection's socket.
struct Outgoing {
    text: String,
    /// One of the chats' events (counted against `EVENT_BACKLOG`).
    event: bool,
}

/// What a machine's login may ask: hello, how to reach the server, then
/// serving as that machine.
fn machine_may(req: &Request) -> bool {
    matches!(req, Request::Hello { .. } | Request::GetServerRoute)
        || tessera_core::engine::is_node_request(req)
}

const NO_ROUTE: &str = "only the Tessera apps' own connections get the server's addresses";

const MACHINE_ONLY: &str =
    "this login belongs to a machine (`tessera node`); it can only serve as that machine";

async fn connection(
    st: Arc<AppState>,
    socket: WebSocket,
    authority: Arc<Authority>,
    revocations: broadcast::Receiver<String>,
    channel: Option<Channel>,
    private: bool,
    route: bool,
) {
    let Login {
        device, machine, ..
    } = authority.login.clone();
    let cancel = CancellationToken::new();
    let monitor = tokio::spawn(watch_revocations(
        st.engine.clone(),
        authority.clone(),
        revocations,
        cancel.clone(),
    ));
    let (mut sink, mut stream) = socket.split();
    let (mut sealer, mut opener): (Option<Sealer>, Option<Opener>) = match channel {
        Some(c) => (Some(c.sealer), Some(c.opener)),
        None => (None, None),
    };
    // Bounded: a client that sends requests but never reads the replies
    // stalls itself (the permits below run out) instead of growing memory.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    // A machine never gets the chats' events (nor copies of them).
    let mut events = (!machine).then(|| st.engine.subscribe());
    // Counted until it closes (`look` waits for every client's answer); a
    // machine shows nothing, so it isn't waited for.
    let _client = (!machine).then(|| st.engine.client_connected());
    // What this connection holds, for `tessera doctor`.
    let stats = tessera_core::engine::ConnStats::register(&device, machine);
    // Web view frames, focus and dialogs for the boards this connection
    // attached to come only to it (dropping the viewer detaches them).
    let (viewer, mut browser_events) = st.engine.browser_viewer();
    let viewer = Arc::new(viewer);
    // If this connection is a machine (`tessera node`), its actions come
    // only to it, and once it is one it no longer gets the chats' events.
    let (node, mut node_events) = st.engine.node_conn(&device);
    let node = Arc::new(node);
    // A required control that cannot be delivered cancels the whole
    // connection, including waits behind a full queue or a held permit.
    let cut_monitor = {
        let node = node.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            watch_cut(cancel, node.cut()).await;
        })
    };
    // The terminals this device opens: their output comes only to this
    // connection (never to the chats' events), and only it can type in them.
    let (term, mut term_events) = st.engine.terminal_conn(&device, private);
    let term = Arc::new(term);

    // Sender: writes what is queued for this connection to its socket, with
    // keepalive pings. On an encrypted connection every message is sealed;
    // pings carry nothing.
    let (wire_tx, mut wire_rx) = mpsc::unbounded_channel::<Outgoing>();
    let event_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Told when something went out: a pump waiting for room (or for a slow
    // device to catch up) looks again, without polling.
    let sent_some = Arc::new(tokio::sync::Notify::new());
    let sender = {
        let (stats, event_bytes, sent_some) =
            (stats.clone(), event_bytes.clone(), sent_some.clone());
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _ = while_live(&cancel, async {
                use std::sync::atomic::Ordering::Relaxed;
                let mut ping = tokio::time::interval(Duration::from_secs(25));
                ping.tick().await;
                loop {
                    let out = tokio::select! {
                        out = wire_rx.recv() => match out {
                            Some(o) => o,
                            None => break,
                        },
                        _ = ping.tick() => {
                            if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break }
                            continue;
                        }
                    };
                    let (len, event) = (out.text.len(), out.event);
                    let sent = match &mut sealer {
                        Some(s) => {
                            let mut ok = true;
                            for frame in s.seal_frames(out.text.as_bytes()) {
                                let Ok(frame) = frame else {
                                    ok = false;
                                    break;
                                };
                                if sink.send(Message::Binary(frame.into())).await.is_err() {
                                    ok = false;
                                    break;
                                }
                            }
                            ok
                        }
                        None => sink.send(Message::Text(out.text.into())).await.is_ok(),
                    };
                    stats.queued_bytes.fetch_sub(len, Relaxed);
                    if event {
                        event_bytes.fetch_sub(len, Relaxed);
                        stats.sent_event(len);
                    }
                    sent_some.notify_one();
                    if !sent {
                        break;
                    }
                }
            })
            .await;
            cancel.cancel();
        })
    };

    // Pump: takes replies, events and frames for this connection and queues
    // them for the sender. The chats' events are taken as they come, however
    // slowly the device reads: a device that falls more than
    // `EVENT_BACKLOG` behind (a phone asleep with its connection open, a
    // slow link) misses events instead of holding them, and is asked to
    // fetch its chats again (`resync`) once it has caught up. So no
    // connection keeps the shared event queue full. Replies and frames wait
    // for the socket instead (`QUEUE_LIMIT`), which slows only this client.
    // Connection-local, deliberately not persisted across an app downgrade.
    let automatic_notifications = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_automatic = automatic_notifications.clone();
    let writer_node = node.clone();
    let (writer_engine, writer_device) = (st.engine.clone(), device.clone());
    let writer = {
        let stats = stats.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _ = while_live(&cancel, async {
            use std::sync::atomic::Ordering::Relaxed;
            let node = writer_node;
            let queue = |text: String, event: bool| {
                stats.queued_bytes.fetch_add(text.len(), Relaxed);
                if event {
                    event_bytes.fetch_add(text.len(), Relaxed);
                }
                wire_tx.send(Outgoing { text, event }).is_ok()
            };
            // Events were dropped: a `resync` is owed once the device caught up.
            let mut behind = false;
            loop {
                if events.is_some() && node.is_node() {
                    events = None;
                    stats.machine.store(true, Relaxed);
                }
                if behind && event_bytes.load(Relaxed) <= EVENT_BACKLOG / 4 {
                    behind = false;
                    let resync = ServerMessage::Event {
                        event: Event::Resync,
                    };
                    let Ok(text) = serde_json::to_string(&resync) else {
                        continue;
                    };
                    if !queue(text, true) {
                        break;
                    }
                }
                let room = stats.queued_bytes.load(Relaxed) < QUEUE_LIMIT;
                let (text, event) = tokio::select! {
                    msg = out_rx.recv(), if room => match msg {
                        Some(m) => (m, false),
                        None => break,
                    },
                    Some(event) = browser_events.recv(), if room => {
                        let Ok(text) = serde_json::to_string(&ServerMessage::Event { event }) else { continue };
                        (text, false)
                    }
                    Some(event) = node_events.recv(), if room => {
                        let Ok(text) = serde_json::to_string(&ServerMessage::Event { event }) else { continue };
                        (text, false)
                    }
                    Some(out) = term_events.recv(), if room => {
                        let Ok(text) = serde_json::to_string(&ServerMessage::Event { event: out.event() }) else { continue };
                        (text, false)
                    }
                    ev = async {
                        match events.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some(rx) = &events {
                            stats.waiting.store(rx.len(), Relaxed);
                        }
                        let event = match ev {
                            Ok(_) if node.is_node() => continue,
                            // An old decoder treats any Notification as an explicit
                            // message, bypassing previews/job preferences. Keep JobUpdated
                            // as its generic, preference-aware fallback instead.
                            Ok(Event::Notification { ref notification })
                                if !notification.kind.is_empty() && !writer_automatic.load(Relaxed) => continue,
                            // Only to the devices that take notifications.
                            Ok(Event::Notification { .. })
                                if !writer_engine.takes_notifications(&writer_device) => continue,
                            // Another device's private chat: nothing of it comes here.
                            Ok(event) if !writer_engine.event_for_device(&event, &writer_device) => continue,
                            Ok(event) => event,
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("client lagged by {n} events; asking it to resync");
                                if !behind {
                                    stats.resyncs.fetch_add(1, Relaxed);
                                }
                                behind = true;
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        };
                        if behind || event_bytes.load(Relaxed) >= EVENT_BACKLOG {
                            if !behind {
                                tracing::info!("a device reads slowly: its events are dropped until it catches up, then it resyncs");
                                stats.resyncs.fetch_add(1, Relaxed);
                            }
                            behind = true;
                            continue;
                        }
                        let Ok(text) = serde_json::to_string(&ServerMessage::Event { event }) else { continue };
                        (text, true)
                    }
                    // Behind (did it catch up?) or full (is there room?): look
                    // again once something went out.
                    _ = sent_some.notified(), if behind || !room => continue,
                };
                if !queue(text, event) {
                    break;
                }
            }
            }).await;
            cancel.cancel();
        })
    };

    // Web view input must reach the page in the order it was typed, and a
    // machine's hello, live output and replies in the order it sent them:
    // one queue per connection, handled one at a time (other requests run
    // concurrently below). Bounded, so a flood stalls only this client.
    let (input_tx, mut input_rx) = mpsc::channel::<(u64, Request)>(256);
    let input_task = {
        let engine = st.engine.clone();
        let tx = out_tx.clone();
        let device = device.clone();
        let viewer = viewer.clone();
        let node = node.clone();
        let term = term.clone();
        let authority = authority.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _ = while_live(&cancel, async {
                while let Some((id, request)) = input_rx.recv().await {
                    let handled = if let Err(e) = authority.check(&engine, &cancel) {
                        Err(e)
                    } else if (machine || node.is_node()) && !machine_may(&request) {
                        Err(ApiError::new(ErrorCode::Unauthorized, MACHINE_ONLY))
                    } else if tessera_core::engine::is_node_request(&request) {
                        engine.node_request(request, &node).await
                    } else if tessera_core::engine::is_terminal_request(&request) {
                        engine.terminal_request(request, &term).await
                    } else {
                        engine
                            .handle_as(request, Some(&device), Some(&viewer))
                            .await
                    };
                    let reply = match handled {
                        Ok(result) => ServerMessage::Response { id, result },
                        Err(error) => ServerMessage::Failure { id, error },
                    };
                    if let Ok(text) = serde_json::to_string(&reply) {
                        let _ = tx.send(text).await;
                    }
                }
            })
            .await;
            cancel.cancel();
        })
    };

    // A device revoked while connected stops at once: nothing more is read,
    // and what it already queued is dropped, not run.
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut requests = tokio::task::JoinSet::new();
    let voice_operations = voice::Operations::default();
    loop {
        let msg = tokio::select! {
            // A revocation wins over a request that arrived at the same moment.
            biased;
            _ = cancel.cancelled() => break,
            _ = requests.join_next(), if !requests.is_empty() => continue,
            msg = stream.next() => match msg {
                Some(Ok(m)) => m,
                _ => break,
            },
        };
        let text = match (msg, &mut opener) {
            (Message::Binary(b), Some(o)) => match o.open(&b) {
                Ok(Some(m)) => match String::from_utf8(m) {
                    Ok(t) => t,
                    Err(_) => break,
                },
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("closing encrypted connection: {e}");
                    break;
                }
            },
            // Never accept plaintext on an encrypted connection.
            (Message::Text(_), Some(_)) => break,
            (Message::Text(t), None) => t.to_string(),
            (Message::Binary(b), None) => String::from_utf8_lossy(&b).to_string(),
            (Message::Close(_), _) => break,
            _ => continue,
        };
        let parsed: Result<ClientMessage, _> = serde_json::from_str(&text);
        // A machine's login, and any connection once it said `node_hello`
        // (a machine paired before machine logins existed), serves only as
        // that machine: no chats, settings or devices through it.
        if let Ok(ClientMessage { id, request }) = &parsed
            && (machine || node.is_node())
            && !machine_may(request)
        {
            let reply = ServerMessage::Failure {
                id: *id,
                error: ApiError::new(ErrorCode::Unauthorized, MACHINE_ONLY),
            };
            if let Ok(text) = serde_json::to_string(&reply)
                && !matches!(while_live(&cancel, out_tx.send(text)).await, Some(Ok(())))
            {
                break;
            }
            continue;
        }
        let parsed = match parsed {
            Ok(ClientMessage { id, request })
                if matches!(request, Request::BrowserInput { .. })
                    || tessera_core::engine::is_node_request(&request)
                    || tessera_core::engine::is_ordered_terminal_request(&request) =>
            {
                if !matches!(
                    while_live(&cancel, input_tx.send((id, request))).await,
                    Some(Ok(()))
                ) {
                    break;
                }
                continue;
            }
            other => other,
        };
        // Stop must bypass the request permits, including when all speech slots are full.
        if let Ok(ClientMessage {
            id,
            request: Request::CancelVoice { operation },
        }) = &parsed
        {
            let reply = match authority.check(&st.engine, &cancel) {
                Ok(()) => {
                    voice_operations.cancel(operation);
                    ServerMessage::Response {
                        id: *id,
                        result: Value::Null,
                    }
                }
                Err(error) => ServerMessage::Failure { id: *id, error },
            };
            if let Ok(text) = serde_json::to_string(&reply) {
                let _ = out_tx.send(text).await;
            }
            continue;
        }
        let speech = matches!(
            &parsed,
            Ok(ClientMessage {
                request: Request::Transcribe { .. } | Request::Speak { .. },
                ..
            })
        );
        let permit = if speech || voice_operations.active() {
            // Other requests can't block reading Stop behind saturated speech slots.
            match in_flight.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    if let Ok(ClientMessage { id, .. }) = &parsed {
                        let reply = ServerMessage::Failure {
                            id: *id,
                            error: ApiError::new(
                                ErrorCode::Busy,
                                "Too many requests; retry in a moment.",
                            ),
                        };
                        if let Ok(text) = serde_json::to_string(&reply) {
                            let _ = out_tx.send(text).await;
                        }
                    }
                    continue;
                }
            }
        } else {
            let Some(Ok(permit)) = while_live(&cancel, in_flight.clone().acquire_owned()).await
            else {
                break;
            };
            permit
        };
        // Register synchronously before reading the next frame; a following cancel can't race admission.
        let voice_operation = match &parsed {
            Ok(ClientMessage { request, .. }) => voice_operations.start(request),
            _ => Ok(None),
        };
        let engine = st.engine.clone();
        let tx = out_tx.clone();
        let device = device.clone();
        let viewer = viewer.clone();
        let term = term.clone();
        let authority = authority.clone();
        let cancel = cancel.clone();
        let node = node.clone();
        let automatic_notifications = automatic_notifications.clone();
        requests.spawn(async move {
            let _permit = permit;
            let _ = while_live(&cancel, async {
                let reply = match parsed {
                    Ok(ClientMessage { id, request }) => {
                        let work = async {
                        if let Err(e) = authority.check(&engine, &cancel) {
                            Err(e)
                        } else if (machine || node.is_node()) && !machine_may(&request) {
                            Err(ApiError::new(ErrorCode::Unauthorized, MACHINE_ONLY))
                        } else if matches!(request, Request::NegotiateNotifications) {
                            automatic_notifications
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            Ok(json!(true))
                        } else if matches!(request, Request::GetServerRoute) {
                            // Credentials (Access headers): only on an encrypted
                            // connection the server decided may have them, and
                            // never logged.
                            if route {
                                serde_json::to_value(engine.server_route())
                                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
                            } else {
                                Err(ApiError::new(ErrorCode::Unauthorized, NO_ROUTE))
                            }
                        } else if tessera_core::engine::is_terminal_request(&request) {
                            engine.terminal_request(request, &term).await
                        } else {
                            engine
                                .handle_as(request, Some(&device), Some(&viewer))
                                .await
                        }
                        };
                        let handled = match voice_operation {
                            Err(error) => Err(error),
                            Ok(None) => work.await,
                            Ok(Some(operation)) => {
                                tokio::select! {
                                    biased;
                                    _ = operation.cancel.cancelled() => Err(ApiError::new(ErrorCode::BadRequest, "Voice operation canceled.")),
                                    result = work => result,
                                }
                            }
                        };
                        match handled {
                            // A machine learns no more than who the server is.
                            Ok(mut result) if machine => {
                                if let Some(info) = result.as_object_mut() {
                                    info.insert("home".into(), json!(""));
                                    info.insert("default_model".into(), Value::Null);
                                }
                                ServerMessage::Response { id, result }
                            }
                            Ok(result) => ServerMessage::Response { id, result },
                            Err(error) => ServerMessage::Failure { id, error },
                        }
                    }
                    Err(e) => {
                        let id = serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
                            .unwrap_or(0);
                        ServerMessage::Failure {
                            id,
                            error: ApiError::new(
                                ErrorCode::BadRequest,
                                format!("invalid request: {e}"),
                            ),
                        }
                    }
                };
                if let Ok(text) = serde_json::to_string(&reply) {
                    let _ = tx.send(text).await;
                }
            })
            .await;
        });
    }
    // No request may outlive its connection, including tasks blocked on a
    // reply, permit, or ordered queue. Await aborts to release their authority.
    cancel.cancel();
    drop(input_tx);
    requests.abort_all();
    input_task.abort();
    cut_monitor.abort();
    writer.abort();
    sender.abort();
    monitor.abort();
    while requests.join_next().await.is_some() {}
    let _ = input_task.await;
    let _ = writer.await;
    let _ = sender.await;
    let _ = monitor.await;
    let _ = cut_monitor.await;
    drop(out_tx);
}

async fn static_files(State(st): State<Arc<AppState>>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(root) = &st.web_dir else {
        return (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><meta charset=utf-8><title>Tessera</title><body style=\"font-family:system-ui;background:#0b0b0c;color:#ececec;display:grid;place-items:center;height:100vh;margin:0\"><div><h1>Tessera server</h1><p>This server is running. Connect with the Tessera app or <code>tessera connect</code>.</p></div>",
        )
            .into_response();
    };
    st.web.serve(root, &uri, &headers, csp).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn request_admission_precedes_bodies_and_only_local_proxies_name_clients() {
        use tower::ServiceExt as _;
        let by_ip = Arc::new(PerIp::default());
        let ip = "203.0.113.9".parse().unwrap();
        let held: Vec<_> = (0..MAX_CONNECTIONS_PER_IP)
            .map(|_| by_ip.take(ip, MAX_CONNECTIONS_PER_IP).unwrap())
            .collect();
        let guard = by_ip.clone();
        let app = Router::new()
            .fallback(|| async { StatusCode::NO_CONTENT })
            .layer(axum::middleware::from_fn(move |req, next| {
                request_body(req, next, guard.clone())
            }));
        for (peer, named, status) in [
            (
                "127.0.0.1:5000",
                "203.0.113.9",
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                "203.0.113.9:5000",
                "198.51.100.4",
                StatusCode::TOO_MANY_REQUESTS,
            ),
            ("127.0.0.1:5000", "198.51.100.4", StatusCode::NO_CONTENT),
        ] {
            let mut req = axum::extract::Request::builder()
                .uri("/unknown")
                .header("cf-connecting-ip", named)
                .body(if status == StatusCode::TOO_MANY_REQUESTS {
                    axum::body::Body::from_stream(futures::stream::pending::<
                        Result<axum::body::Bytes, std::io::Error>,
                    >())
                } else {
                    axum::body::Body::empty()
                })
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            let res = tokio::time::timeout(Duration::from_secs(1), app.clone().oneshot(req))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(res.status(), status);
            if status == StatusCode::TOO_MANY_REQUESTS {
                assert_eq!(res.headers()[header::CONNECTION], "close");
            }
        }
        drop(held);
        assert!(by_ip.0.lock().is_empty());
        let mut req = axum::extract::Request::builder()
            .uri("/unknown")
            .header("cf-connecting-ip", "203.0.113.9")
            .body(axum::body::Body::from(vec![b'x'; MAX_REQUEST_BODY + 1]))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(res.headers()[header::CONNECTION], "close");
        assert!(by_ip.0.lock().is_empty());
        let mut req = axum::extract::Request::builder()
            .uri("/unknown")
            .header("cf-connecting-ip", "203.0.113.9")
            .body(axum::body::Body::from_stream(futures::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >()))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:5000".parse::<SocketAddr>().unwrap()));
        let reading = tokio::spawn(app.oneshot(req));
        tokio::time::timeout(Duration::from_secs(2), async {
            while by_ip.0.lock().get(&ip) != Some(&1) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let res = tokio::time::timeout(BODY_READ_TIMEOUT + Duration::from_secs(2), reading)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(
            by_ip.0.lock().is_empty(),
            "the body deadline returns its address slot"
        );
    }

    #[tokio::test]
    async fn unfinished_request_bodies_close_and_release_admission() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let port = std::env::var("TESSERA_TEST_BODY_PORT").unwrap_or_else(|_| "0".into());
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/api/pair/start",
                post(|Json(_): Json<Value>| async { "ok" }),
            )
            .route(
                "/web/api/pair/start",
                post(|Json(_): Json<Value>| async { "ok" }),
            );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve_until(listener, app, async {
            let _ = stopped.await;
        }));
        for path in ["/api/pair/start", "/web/api/pair/start"] {
            let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            tcp.write_all(format!("POST {path} HTTP/1.1\r\nHost: example.test\r\nCF-Connecting-IP: 203.0.113.9\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{{").as_bytes()).await.unwrap();
            let mut response = Vec::new();
            let read =
                tokio::time::timeout(Duration::from_secs(12), tcp.read_to_end(&mut response)).await;
            assert!(
                matches!(read, Ok(Ok(_))),
                "unfinished body kept its connection open"
            );
            assert!(
                String::from_utf8_lossy(&response).starts_with("HTTP/1.1 408"),
                "{:?}",
                response
            );
        }
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn who_a_request_comes_from() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, HeaderValue::from_static(v));
            }
            h
        };
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:192.168.1.20]:5000".parse().unwrap();
        let cf = headers(&[("cf-connecting-ip", "203.0.113.9")]);
        // Only a tunnel or proxy on this computer may name the client.
        assert_eq!(client_ip(lan, &cf), lan.ip());
        assert_eq!(client_ip(local, &cf).to_string(), "203.0.113.9");
        assert_eq!(client_ip(mapped, &HeaderMap::new()), lan.ip());
        let xff = headers(&[("x-forwarded-for", "10.0.0.1, 198.51.100.4")]);
        assert_eq!(client_ip(local, &xff).to_string(), "198.51.100.4");
        let junk = headers(&[("cf-connecting-ip", "not an address")]);
        assert_eq!(client_ip(local, &junk), local.ip());
    }

    #[test]
    fn a_codes_tries_are_counted_before_they_are_checked() {
        let mut p = Pairing::new();
        assert!(p.try_code("1234-5678"));
        assert!(p.try_code("1234-5678"));
        assert!(p.try_code("1234-5678"));
        assert!(!p.try_code("1234-5678"), "a fourth answer sent at once");
        assert!(p.try_code("8765-4321"), "another code");
    }

    #[test]
    fn slots_per_address_come_back() {
        let map = Arc::new(PerIp::default());
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let a = map.take(ip, 2).unwrap();
        let _b = map.take(ip, 2).unwrap();
        assert!(map.take(ip, 2).is_none(), "full");
        assert!(map.take("203.0.113.10".parse().unwrap(), 2).is_some());
        drop(a);
        assert!(map.take(ip, 2).is_some(), "given back");
    }
}
