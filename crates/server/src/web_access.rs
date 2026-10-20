//! "Use Tessera in a web browser" (off by default; Settings → Connection),
//! everything under `/web`:
//!
//! * `/web/` — the web app (`--web <dir>`, or the one built into the
//!   server), with a strict security policy: nothing from other sites, no
//!   inline script but the page's own, never framed, no referrer.
//! * `/web/api/health`, `/web/api/pair/start`, `/web/api/pair/finish` — the
//!   apps' encrypted pairing, for browsers: each brings a key it made and
//!   can't read out (`PairHello::device_key`), and pairs with a code from one
//!   of the user's devices. Rate-limited apart from the apps, and by address.
//! * `/web/api/secure` — the apps' end-to-end encrypted socket, for paired
//!   browsers only, each proving it holds its key for this very session
//!   (`SessionAuth::proof`). Browsers' logins work nowhere else, and the
//!   apps' logins don't work here.
//!
//! While switched off, all of it answers 404 (nothing is served), and paired
//! browsers' connections are closed. It answers only over https (through a
//! tunnel or proxy on this computer that says so) or to a browser on the
//! server itself: a page that came over plain http could have been changed
//! on the way. Browsers' requests to the API must come from the app's own
//! page: their `Origin` (which no page can choose) must be one of the
//! server's own https addresses, or this computer's own address for a
//! browser on it. So no other site can use a browser's pairing, nor can a
//! page that had its name point at this computer (DNS rebinding).
//!
//! What a proxy on this computer says (https, the browser's address) is
//! believed from any program on it: there is no telling cloudflared from
//! another local program. So the pairing brakes here are per address and
//! never shut the door for everyone: past the door's overall limit each
//! address gets fewer tries, but a fresh one (the owner's) still gets in.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tessera_proto::e2e::{PairFinish, PairStart};

use super::{
    AppState, Door, MAX_HANDSHAKES_PER_IP, client_ip, direct_local, error, finish_pairing, health,
    inline_script_hashes, is_loopback, secure_connection, start_pairing,
};

pub(crate) fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/pair/start", post(pair_start))
        .route("/api/pair/finish", post(pair_finish))
        .route("/api/secure", get(secure_upgrade))
        .fallback(get(files))
        .layer(axum::middleware::from_fn_with_state(state, gate))
}

/// The browser came over https, by a tunnel or proxy on this computer that
/// says so (cloudflared does). Only such a proxy is believed: anything else
/// on the network could claim it.
fn forwarded_https(peer: SocketAddr, headers: &HeaderMap) -> bool {
    if !is_loopback(peer.ip()) {
        return false;
    }
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"));
    let visitor = headers
        .get("cf-visitor")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.replace(' ', "").contains("\"scheme\":\"https\""));
    proto || visitor
}

/// The host the browser asked for: the request's own, or the one a proxy on
/// this computer says it was.
fn asked_host(peer: SocketAddr, headers: &HeaderMap) -> Option<String> {
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
    };
    let forwarded = is_loopback(peer.ip())
        .then(|| text("x-forwarded-host"))
        .flatten();
    let host = forwarded.or_else(|| text(header::HOST.as_str()))?;
    // A plain host name and port; anything else isn't used.
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
        .then_some(host)
}

/// The request comes from the app's own page: its `Origin` is one of the
/// server's own https addresses (`own`: those devices use, and the
/// tunnel's), or, for a browser on this computer itself, the address it
/// asked for. Browsers always send it with these requests (a POST from a
/// script, a WebSocket) and no page can choose it, so a page from another
/// site, or one that had its own name point at this computer, can't pass
/// for ours (whatever `Host` or forwarding headers it adds).
fn same_origin(peer: SocketAddr, headers: &HeaderMap, own: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some((_, rest)) = origin.split_once("://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let ours = own.iter().any(|u| {
        u.strip_prefix("https://")
            .map(|a| a.trim_end_matches('/'))
            .is_some_and(|a| origin.eq_ignore_ascii_case(&format!("https://{a}")))
    });
    ours || (direct_local(peer, headers)
        && asked_host(peer, headers).is_some_and(|h| authority.eq_ignore_ascii_case(&h)))
}

/// The server's own https addresses: those devices use from elsewhere, and
/// its tunnel's.
fn own_addresses(st: &AppState) -> Vec<String> {
    let auth = st.engine.auth();
    let mut own = auth.remote().urls;
    own.extend(auth.tunnel().map(|t| format!("https://{}", t.hostname)));
    own
}

/// The page's security policy: only this server, nothing inline but the
/// page's own scripts, no frames in or out, no forms elsewhere.
fn policy(html: &str, socket: Option<&str>) -> String {
    let hashes = inline_script_hashes(html);
    let socket = socket.map(|s| format!(" {s}")).unwrap_or_default();
    format!(
        "default-src 'none'; script-src 'self'{hashes}; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self'{socket}; manifest-src 'self'; frame-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// The answer for a request under `/web` that may not be answered (none
/// when it may): switched off, nothing at all; plain http from elsewhere,
/// why not; the API from another page, refused. `path` is below `/web`.
fn refusal(st: &AppState, peer: SocketAddr, headers: &HeaderMap, path: &str) -> Option<Response> {
    if !st.engine.web_access_on() {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    if !forwarded_https(peer, headers) && !direct_local(peer, headers) {
        return Some((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "Tessera opens in a web browser only over https (its remote address), or on the server itself. Plain http isn't safe for it: the page could be changed on the way.",
        )
            .into_response());
    }
    if path.starts_with("/api/")
        && path != "/api/health"
        && !same_origin(peer, headers, &own_addresses(st))
    {
        return Some(error(StatusCode::FORBIDDEN, "not from Tessera's own page"));
    }
    None
}

/// The headers every answer under `/web` gets: the page keeps to itself.
fn decorate(mut res: Response, https: bool, api: bool) -> Response {
    let h = res.headers_mut();
    for (name, value) in [
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
        ("x-robots-tag", "noindex, nofollow"),
        (
            "permissions-policy",
            "camera=(self), microphone=(self), geolocation=(), payment=(), usb=(), serial=(), hid=(), bluetooth=(), display-capture=(), publickey-credentials-get=()",
        ),
    ] {
        h.insert(name, HeaderValue::from_static(value));
    }
    if https {
        h.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    if api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// Everything routed under `/web`: admitted, then decorated.
async fn gate(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if let Some(answer) = refusal(&st, peer, req.headers(), &path) {
        return answer;
    }
    let https = forwarded_https(peer, req.headers());
    decorate(next.run(req).await, https, path.starts_with("/api/"))
}

/// The web app's files (the page for any other path).
async fn files(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    serve_files(&st, peer, &uri, &headers).await
}

async fn serve_files(st: &AppState, peer: SocketAddr, uri: &Uri, headers: &HeaderMap) -> Response {
    let socket = asked_host(peer, headers).map(|h| {
        let scheme = if forwarded_https(peer, headers) {
            "wss"
        } else {
            "ws"
        };
        format!("{scheme}://{h}")
    });
    let csp = |html: &str| policy(html, socket.as_deref());
    match &st.web_dir {
        Some(root) => st.web.serve(root, uri, headers, csp).await,
        None => st.web.serve_built(uri, headers, csp),
    }
}

/// `/web` and `/web/` themselves (the router nests only what is below):
/// `/web` → `/web/` (the app finds its files relative to it), and `/web/`
/// is the page, under the same rules as the rest.
pub(crate) async fn top(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if path != "/web" && path != "/web/" {
        return next.run(req).await;
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return match st.engine.web_access_on() {
            true => StatusCode::METHOD_NOT_ALLOWED.into_response(),
            false => StatusCode::NOT_FOUND.into_response(),
        };
    }
    if let Some(answer) = refusal(&st, peer, req.headers(), "/") {
        return answer;
    }
    let https = forwarded_https(peer, req.headers());
    if path == "/web" {
        return decorate(
            (
                StatusCode::PERMANENT_REDIRECT,
                [(header::LOCATION, "/web/")],
            )
                .into_response(),
            https,
            false,
        );
    }
    let root = Uri::from_static("/");
    decorate(
        serve_files(&st, peer, &root, req.headers()).await,
        https,
        false,
    )
}

fn door(peer: SocketAddr, headers: &HeaderMap) -> Door {
    Door::Web(client_ip(peer, headers))
}

async fn pair_start(
    State(st): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PairStart>,
) -> Response {
    start_pairing(&st, req, door(peer, &headers))
}

async fn pair_finish(State(st): State<Arc<AppState>>, Json(req): Json<PairFinish>) -> Response {
    finish_pairing(&st, req, true).await
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
    ws.max_message_size(70 << 10)
        .max_frame_size(70 << 10)
        .on_upgrade(move |socket| secure_connection(st, socket, (permit, slot), true, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn only_a_proxy_on_this_computer_is_believed_about_https() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let cf = headers(&[("cf-visitor", "{\"scheme\":\"https\"}")]);
        let xfp = headers(&[("x-forwarded-proto", "https")]);
        assert!(forwarded_https(local, &cf) && forwarded_https(local, &xfp));
        assert!(!forwarded_https(lan, &cf) && !forwarded_https(lan, &xfp));
        assert!(!forwarded_https(
            local,
            &headers(&[("x-forwarded-proto", "http")])
        ));
        assert!(!forwarded_https(local, &HeaderMap::new()));
    }

    #[test]
    fn the_api_answers_only_the_apps_own_page() {
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.20:5000".parse().unwrap();
        let own = ["https://tessera.example.com".to_string()];
        // The page at one of the server's own https addresses, through the
        // tunnel (whatever `Host` it passes on).
        for host in ["tessera.example.com", "127.0.0.1:7878"] {
            let ours = headers(&[
                ("host", host),
                ("x-forwarded-proto", "https"),
                ("origin", "https://tessera.example.com"),
            ]);
            assert!(same_origin(local, &ours, &own), "{host}");
            assert!(!same_origin(local, &ours, &[]), "{host}");
        }
        assert!(!same_origin(
            local,
            &headers(&[
                ("host", "127.0.0.1:7878"),
                ("origin", "http://tessera.example.com")
            ]),
            &own
        ));
        // Another site's page, whatever it says it asked for: a page that
        // had its own name point at this computer (DNS rebinding) sends its
        // own origin, and may add any forwarding headers it likes.
        for extra in [
            &[][..],
            &[("x-forwarded-host", "evil.example")][..],
            &[("x-forwarded-proto", "https")][..],
        ] {
            let mut theirs =
                headers(&[("host", "evil.example"), ("origin", "http://evil.example")]);
            for (k, v) in extra {
                theirs.insert(*k, HeaderValue::from_static(v));
            }
            assert!(!same_origin(local, &theirs, &own), "{extra:?}");
        }
        assert!(!same_origin(
            local,
            &headers(&[("host", "tessera.example.com")]),
            &own
        ));
        // A browser on this computer itself, at its own address.
        let here = headers(&[
            ("host", "127.0.0.1:7878"),
            ("origin", "http://127.0.0.1:7878"),
        ]);
        assert!(same_origin(local, &here, &[]));
        assert!(!same_origin(lan, &here, &[]));
        let mut proxied = here.clone();
        proxied.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.5"));
        assert!(
            !same_origin(local, &proxied, &[]),
            "not when a proxy brought it"
        );
    }

    #[test]
    fn the_page_may_use_only_this_server() {
        let p = policy(
            "<script>var a = 1;</script><script type=module src=./x.js></script>",
            Some("wss://tessera.example.com"),
        );
        assert!(p.starts_with("default-src 'none';"));
        assert!(p.contains("connect-src 'self' wss://tessera.example.com;"));
        assert!(p.contains("frame-ancestors 'none'") && p.contains("frame-src 'none'"));
        assert!(p.contains("script-src 'self' 'sha256-"), "{p}");
        assert_eq!(p.matches("'sha256-").count(), 1, "only the inline one");
        // No whole schemes or wildcards as sources.
        for source in p.split([' ', ';']) {
            assert!(
                !matches!(
                    source,
                    "http:" | "https:" | "ws:" | "wss:" | "*" | "'unsafe-eval'"
                ),
                "{source} in {p}"
            );
        }
        assert!(!p.contains("script-src 'self' 'unsafe-inline'"));
    }
}
