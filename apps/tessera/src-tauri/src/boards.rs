//! Whiteboards in the app. Board content is untrusted, so:
//! - HTML boards are served from their own origin, with a policy that allows
//!   no network, and the UI frames them sandboxed. Phones use the app's
//!   `tessera-board` scheme; desktops use a server on this device's loopback
//!   (WebKitGTK won't load a custom-scheme document in a sandboxed frame),
//!   where each board's URL carries its own random token;
//! - a `local` board (a web app on the server) gets a proxy on this device's
//!   loopback, one port per board, unusable without its random token; opened
//!   in the browser it is addressed by a random `*.localhost` name of its own,
//!   so it shares no cookies with other services on this computer;
//! - previews of a note's HTML are served like HTML boards, each under its
//!   own random token, from memory (the last few only).

use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use parking_lot::Mutex;
use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Manager, State};
use tessera_client::Client;
use tessera_proto::{BoardContent, BoardResponse, Whiteboard};

use crate::AppState;

pub const SCHEME: &str = "tessera-board";
/// Where phones' web views reach the board scheme.
const PHONE_ORIGIN: &str = "http://tessera-board.localhost";

fn client(app: &AppHandle) -> Option<Client> {
    app.try_state::<AppState>()?
        .conn
        .lock()
        .as_ref()
        .map(|(_, c)| c.clone())
}

fn respond(status: StatusCode, mime: &str, body: impl Into<Vec<u8>>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("content-type", mime)
        .header("cache-control", "no-store")
        .body(body.into())
        .unwrap_or_default()
}

/// `http://tessera-board.localhost/board/<id>/<version>/?theme=light` and
/// `/board-lib/<file>` (phones).
pub fn protocol(app: AppHandle, request: Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    tauri::async_runtime::spawn(async move {
        responder.respond(serve(&app, &request).await);
    });
}

async fn serve(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path();
    if let Some(lib) = path.strip_prefix("/board-lib/") {
        return lib_file(lib);
    }
    let query = request.uri().query().unwrap_or("");
    if let Some(token) = path.strip_prefix("/preview/") {
        return preview(app, token, query, PHONE_ORIGIN);
    }
    let Some(id) = board_id(path) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", "not found");
    };
    let Some(version) = board_version(path) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", "not found");
    };
    document(app, id, version, query, PHONE_ORIGIN).await
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The board id in `/board/<id>/<version>/`.
fn board_id(path: &str) -> Option<&str> {
    if !tessera_proto::board::frame_path(path) {
        return None;
    }
    let mut parts = path.strip_prefix("/board/")?.split('/');
    let id = parts.next()?;
    let version = parts.next()?;
    (valid_id(id) && version.parse::<u64>().is_ok()).then_some(id)
}

fn board_version(path: &str) -> Option<u64> {
    path.trim_matches('/').split('/').nth(1)?.parse().ok()
}

/// A bundled library (three.js, d3…); nothing private, so no token.
fn lib_file(path: &str) -> Response<Vec<u8>> {
    match tessera_proto::board::lib(path) {
        Some((bytes, mime)) => Response::builder()
            .header("content-type", mime)
            // Boards run on an opaque origin: module imports need CORS.
            .header("access-control-allow-origin", "*")
            .header("cache-control", "public, max-age=86400")
            .body(bytes.to_vec())
            .unwrap_or_default(),
        None => respond(StatusCode::NOT_FOUND, "text/plain", "not found"),
    }
}

/// The board's HTML, fetched over the encrypted connection, in the page that
/// allows no network.
async fn document(
    app: &AppHandle,
    id: &str,
    version: u64,
    query: &str,
    origin: &str,
) -> Response<Vec<u8>> {
    let Some(client) = client(app) else {
        return respond(
            StatusCode::SERVICE_UNAVAILABLE,
            "text/plain",
            "not connected",
        );
    };
    let board = client
        .call_as::<Whiteboard>(tessera_proto::Request::GetBoard { id: id.to_string() })
        .await;
    let Ok(Whiteboard {
        content: BoardContent::Html { html },
        version: current,
        ..
    }) = board
    else {
        return respond(StatusCode::NOT_FOUND, "text/plain", "no such whiteboard");
    };
    if current != version {
        return respond(
            StatusCode::NOT_FOUND,
            "text/plain",
            "this whiteboard link has expired",
        );
    }
    page(&html, query, origin)
}

/// Untrusted HTML in the page that allows no network.
fn page(html: &str, query: &str, origin: &str) -> Response<Vec<u8>> {
    let dark = !query.contains("theme=light");
    Response::builder()
        .header("content-type", "text/html; charset=utf-8")
        .header(
            "content-security-policy",
            tessera_proto::board::csp(Some(origin)),
        )
        .header("referrer-policy", "no-referrer")
        .header("cache-control", "no-store")
        .body(tessera_proto::board::document(html, dark, Some(origin)).into_bytes())
        .unwrap_or_default()
}

/// Previews of a note's HTML: token and HTML, oldest first (the last few).
#[derive(Default)]
pub struct Previews(Mutex<VecDeque<(String, String)>>);

const MAX_PREVIEWS: usize = 16;
const MAX_PREVIEW: usize = 512 << 10;

impl Previews {
    /// Keep `html` under a new random token.
    fn add(&self, html: String) -> String {
        let token = random_token();
        let mut p = self.0.lock();
        while p.len() >= MAX_PREVIEWS {
            p.pop_front();
        }
        p.push_back((token.clone(), html));
        token
    }

    fn get(&self, token: &str) -> Option<String> {
        self.0
            .lock()
            .iter()
            .find(|(t, _)| eq(t, token))
            .map(|(_, h)| h.clone())
    }
}

/// `/preview/<token>/`: a preview the UI handed over.
fn preview(app: &AppHandle, token: &str, query: &str, origin: &str) -> Response<Vec<u8>> {
    let token = token.trim_end_matches('/');
    match app.try_state::<Previews>().and_then(|p| p.get(token)) {
        Some(html) => page(&html, query, origin),
        None => respond(
            StatusCode::NOT_FOUND,
            "text/plain",
            "this preview has expired",
        ),
    }
}

/// Where the UI frames a preview of `html` (a note's HTML block): the board
/// origin, like an HTML board, under a new random token.
#[tauri::command]
pub async fn preview_frame_url(
    app: AppHandle,
    frames: State<'_, Frames>,
    previews: State<'_, Previews>,
    html: String,
) -> Result<String, String> {
    if html.len() > MAX_PREVIEW {
        return Err("this is too big to preview".into());
    }
    let token = previews.add(html);
    if cfg!(mobile) {
        return Ok(format!("{PHONE_ORIGIN}/preview/{token}/"));
    }
    let port = frames.port(&app).await?;
    Ok(format!("http://127.0.0.1:{port}/preview/{token}/"))
}

/// Where the UI frames an HTML board: the board scheme on phones, this
/// device's loopback server (with the board's token) on desktops.
#[tauri::command]
pub async fn board_frame_url(
    app: AppHandle,
    frames: State<'_, Frames>,
    id: String,
    version: u64,
) -> Result<String, String> {
    if !valid_id(&id) {
        return Err("bad board id".into());
    }
    if cfg!(mobile) {
        return Ok(frame_url(PHONE_ORIGIN, &id, version, None));
    }
    let port = frames.port(&app).await?;
    let token = frames.token(&id, version);
    Ok(frame_url(
        &format!("http://127.0.0.1:{port}"),
        &id,
        version,
        Some(&token),
    ))
}

fn frame_url(origin: &str, id: &str, version: u64, token: Option<&str>) -> String {
    let url = format!("{origin}/board/{id}/{version}/");
    match token {
        Some(t) => with_token(&url, t),
        None => url,
    }
}

/// Desktop: the loopback server for HTML boards (started on first use, one
/// port for the app) and each board's token.
#[derive(Default)]
pub struct Frames {
    port: tokio::sync::OnceCell<u16>,
    tokens: Mutex<HashMap<String, FrameGrant>>,
}

struct FrameGrant {
    token: String,
    version: u64,
    expires: std::time::Instant,
}

impl Frames {
    async fn port(&self, app: &AppHandle) -> Result<u16, String> {
        self.port
            .get_or_try_init(|| async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(|e| e.to_string())?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                tauri::async_runtime::spawn(serve_frames(app.clone(), listener, port));
                Ok(port)
            })
            .await
            .copied()
    }

    fn token(&self, id: &str, version: u64) -> String {
        self.token_at(id, version, std::time::Instant::now())
    }

    fn token_at(&self, id: &str, version: u64, now: std::time::Instant) -> String {
        let mut tokens = self.tokens.lock();
        let grant = tokens.entry(id.to_string()).or_insert_with(|| FrameGrant {
            token: random_token(),
            version,
            expires: now + std::time::Duration::from_secs(86_400),
        });
        if grant.version != version || grant.expires <= now {
            *grant = FrameGrant {
                token: random_token(),
                version,
                expires: now + std::time::Duration::from_secs(86_400),
            };
        }
        grant.token.clone()
    }

    fn allows(&self, id: &str, version: u64, token: Option<&str>) -> bool {
        self.allows_at(id, version, token, std::time::Instant::now())
    }

    fn allows_at(
        &self,
        id: &str,
        version: u64,
        token: Option<&str>,
        now: std::time::Instant,
    ) -> bool {
        self.tokens.lock().get(id).is_some_and(|g| {
            g.version == version && g.expires > now && token.is_some_and(|t| eq(&g.token, t))
        })
    }
}

/// Whether a navigation goes to one of the frame servers this app started on
/// the loopback (HTML boards, local web apps). The navigation guard also sees
/// link clicks inside frames on Linux, and only the app hands out these ports.
pub fn is_frame_url<R: tauri::Runtime>(app: &AppHandle<R>, url: &tauri::Url) -> bool {
    if matches!(url.scheme(), "http" | "tessera-board")
        && matches!(
            url.host_str(),
            Some("tessera-board.localhost" | "localhost")
        )
        && url.port().is_none()
        && tessera_proto::board::frame_path(url.path())
    {
        return url.scheme() == "tessera-board"
            || url.host_str() == Some("tessera-board.localhost");
    }
    let Some(port) = url.port() else {
        return false;
    };
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") {
        return false;
    }
    (tessera_proto::board::frame_path(url.path())
        && app
            .try_state::<Frames>()
            .is_some_and(|f| f.port.get() == Some(&port)))
        || app
            .try_state::<Proxies>()
            .is_some_and(|p| p.0.lock().values().any(|(p, ..)| *p == port))
}

async fn serve_frames(app: AppHandle, listener: tokio::net::TcpListener, port: u16) {
    let host = format!("127.0.0.1:{port}");
    let router = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let (app, host) = (app.clone(), host.clone());
        async move { frame(&app, &host, req).await.map(axum::body::Body::from) }
    });
    let _ = axum::serve(listener, router).await;
}

/// `/board/<id>/<version>/?__tessera=<token>` (a board's page, only with that
/// board's token), `/preview/<token>/` and `/board-lib/<file>`.
async fn frame(app: &AppHandle, host: &str, req: axum::extract::Request) -> Response<Vec<u8>> {
    // Only addressed as this loopback port: a name pointed here from elsewhere gets nothing.
    if req.headers().get("host").and_then(|h| h.to_str().ok()) != Some(host) {
        return respond(StatusCode::MISDIRECTED_REQUEST, "text/plain", "wrong host");
    }
    if !matches!(req.method().as_str(), "GET" | "HEAD") {
        return respond(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "not allowed");
    }
    let path = req.uri().path();
    if let Some(lib) = path.strip_prefix("/board-lib/") {
        return lib_file(lib);
    }
    if let Some(token) = path.strip_prefix("/preview/") {
        let query = req.uri().query().unwrap_or("");
        return preview(app, token, query, &format!("http://{host}"));
    }
    let pq = req.uri().path_and_query().map_or("/", |p| p.as_str());
    let (clean, token) = split_token(pq);
    let Some(id) = board_id(path) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", "not found");
    };
    let Some(version) = board_version(path) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", "not found");
    };
    if !app.state::<Frames>().allows(id, version, token.as_deref()) {
        return respond(StatusCode::FORBIDDEN, "text/plain", "forbidden");
    }
    let query = clean.split_once('?').map_or("", |(_, q)| q);
    document(app, id, version, query, &format!("http://{host}")).await
}

/// Device-side proxies for `local` boards: port, token and host name by
/// board id.
#[derive(Default)]
pub struct Proxies(Mutex<HashMap<String, (u16, String, String)>>);

/// Start (or reuse) this board's proxy; the URL carries its token. For the
/// browser (`external`), the URL names the proxy by its own `*.localhost`
/// host, so the page shares no cookies with other services on 127.0.0.1.
#[tauri::command]
pub async fn board_local_url(
    app: AppHandle,
    proxies: State<'_, Proxies>,
    id: String,
    external: Option<bool>,
) -> Result<String, String> {
    let client = client(&app).ok_or("not connected to a server")?;
    let board = client
        .call_as::<Whiteboard>(tessera_proto::Request::GetBoard { id: id.clone() })
        .await
        .map_err(|e| e.message)?;
    let BoardContent::Local { path, .. } = board.content else {
        return Err("that whiteboard doesn't show a local web app".into());
    };
    let existing = proxies.0.lock().get(&id).cloned();
    let (port, token, label) = match existing {
        Some(p) => p,
        None => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| e.to_string())?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            let token = random_token();
            let label = random_label();
            let proxy = Proxy {
                app: app.clone(),
                board: id.clone(),
                token: token.clone(),
                port,
                label: label.clone(),
            };
            tauri::async_runtime::spawn(proxy.run(listener));
            proxies
                .0
                .lock()
                .insert(id.clone(), (port, token.clone(), label.clone()));
            (port, token, label)
        }
    };
    let host = if external == Some(true) {
        format!("{label}.localhost")
    } else {
        "127.0.0.1".to_string()
    };
    Ok(with_token(&format!("http://{host}:{port}{path}"), &token))
}

/// A host name of its own for a proxy (`<label>.localhost`).
fn random_label() -> String {
    use rand::RngCore;
    let mut b = [0u8; 10];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A proxy answers only to the names it is given out under: a name pointed
/// at 127.0.0.1 from elsewhere (DNS rebinding) gets nothing.
fn proxy_host_ok(host: Option<&str>, port: u16, label: &str) -> bool {
    host.is_some_and(|h| {
        h.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
            || h.eq_ignore_ascii_case(&format!("{label}.localhost:{port}"))
    })
}

fn random_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 18];
    rand::rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

const TOKEN_PARAM: &str = "__tessera";

fn with_token(url: &str, token: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{TOKEN_PARAM}={token}")
}

/// The token in a URL's query, and the query without it.
fn split_token(path_and_query: &str) -> (String, Option<String>) {
    let Some((path, query)) = path_and_query.split_once('?') else {
        return (path_and_query.to_string(), None);
    };
    let mut token = None;
    let rest: Vec<&str> = query
        .split('&')
        .filter(|kv| match kv.strip_prefix(&format!("{TOKEN_PARAM}=")) {
            Some(t) => {
                token = Some(t.to_string());
                false
            }
            None => true,
        })
        .collect();
    let clean = if rest.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", rest.join("&"))
    };
    (clean, token)
}

struct Proxy {
    app: AppHandle,
    board: String,
    token: String,
    port: u16,
    label: String,
}

impl Proxy {
    async fn run(self, listener: tokio::net::TcpListener) {
        let me = std::sync::Arc::new(self);
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let me = me.clone();
            async move { me.handle(req).await }
        });
        let _ = axum::serve(listener, app).await;
    }

    /// Only requests carrying the token (in the URL, or in the Referer of a
    /// page that had it) get through; pages keep it in their URL so the
    /// Referer of their subresources carries it.
    async fn handle(&self, req: axum::extract::Request) -> axum::response::Response {
        use axum::response::IntoResponse;
        let (parts, body) = req.into_parts();
        let host = parts.headers.get("host").and_then(|h| h.to_str().ok());
        if !proxy_host_ok(host, self.port, &self.label) {
            return (StatusCode::MISDIRECTED_REQUEST, "wrong host").into_response();
        }
        let pq = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/");
        let (clean, in_url) = split_token(pq);
        let from_referer = parts
            .headers
            .get("referer")
            .and_then(|v| v.to_str().ok())
            .and_then(|r| r.split_once('?').map(|(_, q)| q.to_string()))
            .and_then(|q| {
                q.split('&').find_map(|kv| {
                    kv.strip_prefix(&format!("{TOKEN_PARAM}="))
                        .map(str::to_string)
                })
            });
        let ok = |t: &Option<String>| t.as_deref().is_some_and(|t| eq(t, &self.token));
        if !ok(&in_url) {
            if !ok(&from_referer) {
                return (StatusCode::FORBIDDEN, "forbidden").into_response();
            }
            // A page navigated to without the token: put it back in its URL.
            let navigation = parts
                .headers
                .get("sec-fetch-dest")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|d| matches!(d, "document" | "iframe" | "frame"))
                || parts
                    .headers
                    .get("accept")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|a| a.starts_with("text/html"));
            if navigation && parts.method == axum::http::Method::GET {
                return axum::response::Redirect::temporary(&with_token(&clean, &self.token))
                    .into_response();
            }
        }
        let Some(client) = client(&self.app) else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "not connected to the server",
            )
                .into_response();
        };
        let body = match axum::body::to_bytes(body, 20 << 20).await {
            Ok(b) if !b.is_empty() => Some(base64::engine::general_purpose::STANDARD.encode(&b)),
            Ok(_) => None,
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "too large").into_response(),
        };
        let headers = parts
            .headers
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "referer" | "host" | "origin" | "cookie"))
            .filter_map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let resp = client
            .call_as::<BoardResponse>(tessera_proto::Request::BoardFetch {
                id: self.board.clone(),
                method: parts.method.to_string(),
                path: clean,
                headers,
                body,
            })
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return (StatusCode::BAD_GATEWAY, e.message).into_response(),
        };
        let mut out = axum::http::Response::builder()
            .status(StatusCode::from_u16(resp.status).unwrap_or(StatusCode::BAD_GATEWAY));
        for (k, v) in &resp.headers {
            let lk = k.to_ascii_lowercase();
            // Let it be framed by the app, keep Referers within the page
            // (they carry the token), and keep redirects on this proxy.
            if matches!(
                lk.as_str(),
                "x-frame-options" | "content-security-policy" | "referrer-policy" | "set-cookie"
            ) {
                continue;
            }
            if lk == "location" {
                out = out.header(k, local_location(v, &self.token));
                continue;
            }
            out = out.header(k, v);
        }
        out = out.header("referrer-policy", "same-origin");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(resp.body)
            .unwrap_or_default();
        out.body(axum::body::Body::from(bytes))
            .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "bad response").into_response())
    }
}

/// A redirect from the web app: keep it on this proxy, with the token.
fn local_location(v: &str, token: &str) -> String {
    let path = if let Some(rest) = v
        .strip_prefix("http://127.0.0.1")
        .or(v.strip_prefix("http://localhost"))
    {
        // Drop the app's own port: the path stays on this proxy.
        rest.find('/').map_or("/", |i| &rest[i..]).to_string()
    } else if v.starts_with('/') {
        v.to_string()
    } else {
        return v.to_string();
    };
    with_token(&path, token)
}

fn eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_split_from_urls_and_redirects_stay_local() {
        assert_eq!(
            split_token("/a?b=1&__tessera=xyz"),
            ("/a?b=1".to_string(), Some("xyz".into()))
        );
        assert_eq!(
            split_token("/a?__tessera=xyz"),
            ("/a".to_string(), Some("xyz".into()))
        );
        assert_eq!(split_token("/a"), ("/a".to_string(), None));
        assert_eq!(with_token("/x", "t"), "/x?__tessera=t");
        assert_eq!(with_token("/x?y=1", "t"), "/x?y=1&__tessera=t");
        assert_eq!(local_location("/login", "t"), "/login?__tessera=t");
        assert_eq!(
            local_location("http://127.0.0.1:5173/app", "t"),
            "/app?__tessera=t"
        );
        assert_eq!(
            local_location("https://example.com/", "t"),
            "https://example.com/"
        );
        assert!(eq("abc", "abc") && !eq("abc", "abd") && !eq("ab", "abc"));
        let a = random_token();
        assert!(a.len() >= 24 && a != random_token());
    }

    #[test]
    fn board_frames_live_on_their_own_origin() {
        assert_eq!(
            frame_url("http://tessera-board.localhost", "wb_abc123", 3, None),
            "http://tessera-board.localhost/board/wb_abc123/3/"
        );
        assert_eq!(
            frame_url("http://127.0.0.1:4000", "wb_abc123", 3, Some("t")),
            "http://127.0.0.1:4000/board/wb_abc123/3/?__tessera=t"
        );
        assert_eq!(board_id("/board/wb_abc123/3/"), Some("wb_abc123"));
        assert_eq!(board_id("/../x/1/"), None);
        assert_eq!(board_id("/"), None);
        assert!(!valid_id("../x") && !valid_id("") && valid_id("wb_1"));
    }

    #[test]
    fn previews_are_kept_by_their_token_and_only_the_last_few() {
        let p = Previews::default();
        let first = p.add("<p>first</p>".into());
        assert_eq!(p.get(&first).as_deref(), Some("<p>first</p>"));
        assert!(p.get("nope").is_none() && p.get("").is_none());
        for i in 0..MAX_PREVIEWS {
            p.add(format!("<p>{i}</p>"));
        }
        assert!(p.get(&first).is_none(), "the oldest went");
        let r = page("<p>x</p>", "theme=light", "http://127.0.0.1:4000");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("connect-src 'none'"), "{csp}");
        let body = String::from_utf8_lossy(r.body());
        assert!(body.contains("<p>x</p>") && body.contains("color-scheme:light"));
    }

    #[test]
    fn a_proxy_answers_only_to_its_own_names() {
        let label = random_label();
        assert_eq!(label.len(), 20);
        assert!(label.bytes().all(|b| b.is_ascii_hexdigit()) && label != random_label());
        assert!(proxy_host_ok(Some("127.0.0.1:4100"), 4100, &label));
        assert!(proxy_host_ok(
            Some(&format!("{label}.localhost:4100")),
            4100,
            &label
        ));
        for bad in [
            None,
            Some("127.0.0.1:4101".to_string()),
            Some("localhost:4100".to_string()),
            Some("evil.example:4100".to_string()),
            Some(format!("{label}.localhost.evil.example:4100")),
            Some("other.localhost:4100".to_string()),
        ] {
            assert!(!proxy_host_ok(bad.as_deref(), 4100, &label), "{bad:?}");
        }
    }

    #[test]
    fn a_board_page_needs_its_own_token() {
        let f = Frames::default();
        let a = f.token("wb_a", 1);
        assert_eq!(f.token("wb_a", 1), a);
        let b = f.token("wb_b", 1);
        assert!(f.allows("wb_a", 1, Some(&a)));
        assert!(!f.allows("wb_a", 1, Some(&b)));
        assert!(!f.allows("wb_a", 1, None));
        assert!(!f.allows("wb_c", 1, Some(&a)));
        let now = std::time::Instant::now();
        let expiry = now + std::time::Duration::from_secs(86_400);
        let token = f.token_at("wb_time", 1, now);
        assert!(f.allows_at(
            "wb_time",
            1,
            Some(&token),
            expiry - std::time::Duration::from_nanos(1)
        ));
        assert!(!f.allows_at("wb_time", 1, Some(&token), expiry));
        let fresh = f.token_at("wb_time", 1, expiry);
        assert_ne!(token, fresh);
        assert!(!f.allows_at("wb_time", 1, Some(&token), expiry));
        assert!(f.allows_at("wb_time", 1, Some(&fresh), expiry));
        let revised = f.token_at("wb_time", 2, expiry);
        assert_ne!(fresh, revised);
        assert!(!f.allows_at("wb_time", 1, Some(&fresh), expiry));
    }
}
