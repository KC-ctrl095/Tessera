//! The server-side browser end to end with a real Chromium: web view tiles
//! (frames to the attached connection only, input, navigation, popups,
//! dialogs, focus), the agent's `browser` tool (snapshots, refs, typing,
//! approvals, secrets, downloads) and recovery. Skipped when no
//! Chromium-family browser is installed.

mod common;
#[path = "browser/normal.rs"]
mod normal;
#[path = "browser/uploads.rs"]
mod uploads;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use common::*;
use parking_lot::Mutex;
use tessera_proto::*;
use tokio::sync::mpsc;

const START: &str = r#"<!doctype html><html><head><title>Start</title><meta name="viewport" content="width=device-width">
<style>.fx{position:fixed;display:block;height:30px;top:300px;margin:0;padding:0;border:0;font:14px sans-serif}</style></head><body>
<h1>Test page</h1>
<p>Some text for the snapshot.</p>
<form id="f" method="post" action="/submitted">
  <label>Name <input id="name" name="name"></label>
  <label>Password <input id="pw" type="password" name="pw"></label>
  <button>Sign in</button>
</form>
<input id="q" class="fx" style="left:10px;width:200px" placeholder="Search" oninput="document.title='typed:'+this.value">
<a href="/two" target="_blank" class="fx" style="left:250px;width:120px">Popup link</a>
<button id="ask" class="fx" style="left:400px;width:100px" onclick="document.title = confirm('Sure?') ? 'yes' : 'no'">Ask me</button>
<a href="/two" id="same">Second page</a>
<a href="/file.txt" download>Get file</a>
<select id="s"><option>Red</option><option>Green</option></select>
<div style="height:2000px"></div><p id="far">Far below</p>
</body></html>"#;

/// A sign-in page that offers a security key when the browser has them
/// (as sites do), asks for one either way, and has a password form.
const KEYS: &str = r#"<!doctype html><html><head><title>Keys</title><meta name="viewport" content="width=device-width"></head><body>
<form id="f" method="post" action="/submitted">
  <input id="name" name="name" style="position:fixed;left:10px;top:20px;width:200px;height:30px;margin:0;border:0;padding:0" autocomplete="username">
  <input id="pw" type="password" name="pw" style="position:fixed;left:10px;top:70px;width:200px;height:30px;margin:0;border:0;padding:0" autocomplete="current-password">
  <button style="position:fixed;left:10px;top:120px;width:200px;height:30px;margin:0;border:0;padding:0">Sign in</button>
</form>
<p id="offer"></p>
<script>
(async () => {
  const out = ['offer:' + (window.PublicKeyCredential ? 'key' : 'password')];
  const started = performance.now();
  const ask = async (what, options) => {
    try { await navigator.credentials[what]({ publicKey: options }); out.push(what + ':ok'); }
    catch (e) { out.push(what + ':' + e.name); }
  };
  const challenge = new Uint8Array(32);
  await ask('get', { challenge, timeout: 60000, rpId: location.hostname, allowCredentials: [{ type: 'public-key', id: new Uint8Array(16) }] });
  await ask('create', { challenge, timeout: 60000, rp: { name: 'Test' }, user: { id: new Uint8Array(8), name: 'ann', displayName: 'Ann' }, pubKeyCredParams: [{ type: 'public-key', alg: -7 }] });
  out.push(performance.now() - started < 5000 ? 'at-once' : 'slow');
  document.title = out.join(' ');
})();
</script></body></html>"#;

/// A small site for the browser: http://127.0.0.1:<port>/
async fn site() -> (String, Arc<Mutex<Vec<String>>>) {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let p = posted.clone();
    let app = Router::new()
        .route("/", get(|| async { axum::response::Html(START) }))
        .route(
            "/two",
            get(|| async {
                axum::response::Html(
                    "<!doctype html><title>Two</title><h2>Second</h2><p>Page two text</p>",
                )
            }),
        )
        .route(
            "/file.txt",
            get(|| async {
                (
                    [
                        ("content-type", "text/plain"),
                        ("content-disposition", "attachment; filename=\"notes.txt\""),
                    ],
                    "file body",
                )
                    .into_response()
            }),
        )
        .route(
            "/submitted",
            post(move |body: String| {
                let p = p.clone();
                async move {
                    p.lock().push(body);
                    axum::response::Html("<!doctype html><title>Done</title><p>Signed in</p>")
                }
            }),
        );
    let app = app
        .merge(oauth_routes())
        .route("/keys", get(|| async { axum::response::Html(KEYS) }))
        .route(
            "/challenge",
            get(|| async {
                axum::response::Html(
                    "<!doctype html><title>Sign in</title><form><input name=user><button>Sign in</button></form><p class=error>Please complete the verification challenge and try again.</p>",
                )
            }),
        )
        // Asks for the check only after a sign-in attempt, on the same page.
        .route(
            "/challenge-later",
            get(|| async {
                axum::response::Html(
                    "<!doctype html><title>Sign in later</title><button id=b style=\"position:fixed;left:10px;top:20px;width:200px;height:40px\" onclick=\"document.body.insertAdjacentHTML('beforeend', '<p>Please complete the verification challenge and try again.</p>')\">Sign in</button>",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), posted)
}

/// A sign-in provider on the same site: a popup flow (the popup posts the
/// code to its opener and closes itself) and a redirect flow (a session
/// cookie, then the account page).
fn oauth_routes() -> Router {
    const FX: &str = "position:fixed;left:10px;top:300px;width:220px;height:30px;margin:0;padding:0;border:0;font:14px sans-serif";
    Router::new()
        .route(
            "/login",
            get(|| async {
                axum::response::Html(format!(
                    r#"<!doctype html><title>Login</title><meta name="viewport" content="width=device-width">
<p id="status">Signed out</p>
<button style="{FX}" onclick="window.open('/oauth/authorize?mode=popup','oauth','width=480,height=640')">Sign in with Provider</button>
<script>addEventListener('message', e => {{
  if (e.origin === location.origin && e.data && e.data.code) {{
    document.title = 'logged in: ' + e.data.code;
    document.getElementById('status').textContent = 'Logged in as Ann';
  }}
}});</script>"#
                ))
            }),
        )
        .route(
            "/oauth/authorize",
            get(|q: axum::extract::RawQuery, headers: axum::http::HeaderMap| async move {
                let mode = if q.0.unwrap_or_default().contains("redirect") { "redirect" } else { "popup" };
                // What the first request of a popup says it is.
                let ua = headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .replace('<', "");
                axum::response::Html(format!(
                    r#"<!doctype html><title>Provider</title><meta name="viewport" content="width=device-width">
<h1>Allow access?</h1><p>UA: {ua}</p>
<form method="post" action="/oauth/approve"><input type="hidden" name="mode" value="{mode}">
<button style="{FX}">Allow</button></form>"#
                ))
            }),
        )
        .route(
            "/oauth/approve",
            post(|body: String| async move {
                let to = if body.contains("redirect") { "/cb?code=xyz" } else { "/oauth/callback?code=xyz" };
                axum::response::Redirect::to(to)
            }),
        )
        .route(
            "/oauth/callback",
            get(|| async {
                axum::response::Html(
                    r#"<!doctype html><title>Callback</title><script>
window.opener.postMessage({code: new URLSearchParams(location.search).get('code')}, location.origin);
setTimeout(() => window.close(), 200);
</script><p>Returning…</p>"#,
                )
            }),
        )
        .route(
            "/cb",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [
                        ("set-cookie", "sid=ann; Max-Age=3600; Path=/; HttpOnly"),
                        ("location", "/account"),
                    ],
                )
            }),
        )
        .route(
            "/account",
            get(|headers: axum::http::HeaderMap| async move {
                let signed_in = headers
                    .get("cookie")
                    .and_then(|c| c.to_str().ok())
                    .is_some_and(|c| c.contains("sid=ann"));
                axum::response::Html(if signed_in {
                    "<!doctype html><title>Account</title><p>Welcome Ann</p>"
                } else {
                    "<!doctype html><title>Signed out</title><p>Please sign in</p>"
                })
            }),
        )
}

fn have_browser() -> bool {
    let ok = tessera_core::browser::installed(None);
    if !ok {
        eprintln!("no Chromium-family browser installed; skipping");
    }
    ok
}

async fn board(h: &Harness, session: &str, url: &str) -> Whiteboard {
    let v = h
        .engine
        .handle(
            Request::CreateBoard {
                session_id: session.into(),
                title: "Web".into(),
                placed: false,
                content: BoardContent::Url { url: url.into() },
            },
            None,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// The next event matching `f` within `secs`.
async fn next<T>(
    rx: &mut mpsc::Receiver<Event>,
    secs: u64,
    mut f: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            match rx.recv().await {
                Some(ev) => {
                    if let Some(t) = f(&ev) {
                        return Some(t);
                    }
                }
                None => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Pixel size of a base64 JPEG (from its frame header).
fn jpeg_size(b64: &str) -> (u32, u32) {
    use base64::Engine as _;
    let b = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    let mut i = 2;
    while i + 9 < b.len() && b[i] == 0xFF {
        let m = b[i + 1];
        if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return (w, h);
        }
        i += 2 + u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
    }
    (0, 0)
}

/// The next frame for `board` (acknowledged).
async fn frame(
    h: &Harness,
    rx: &mut mpsc::Receiver<Event>,
    viewer: &tessera_core::browser::Viewer,
    board: &str,
) -> Option<(u64, usize, u32, u32)> {
    let f = next(rx, 15, |e| match e {
        Event::BrowserFrame {
            board_id,
            frame,
            data,
            width,
            height,
            ..
        } if board_id == board => Some((*frame, data.len(), *width, *height)),
        _ => None,
    })
    .await?;
    h.engine
        .handle_as(
            Request::BrowserAck {
                board_id: board.into(),
                frame: f.0,
            },
            None,
            Some(viewer),
        )
        .await
        .unwrap();
    Some(f)
}

async fn call(
    h: &Harness,
    viewer: &tessera_core::browser::Viewer,
    req: Request,
) -> serde_json::Value {
    h.engine.handle_as(req, None, Some(viewer)).await.unwrap()
}

async fn title_becomes(h: &Harness, board: &str, want: &str) -> bool {
    state_becomes(h, board, |t| t.title == want).await
}

/// Wait (up to 10 s) until the board's tab state satisfies `f`.
async fn state_becomes(h: &Harness, board: &str, f: impl Fn(&BrowserTab) -> bool) -> bool {
    state_within(h, board, 10, f).await
}

/// Wait up to `secs` until the board's tab state satisfies `f`.
async fn state_within(
    h: &Harness,
    board: &str,
    secs: u64,
    f: impl Fn(&BrowserTab) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut events = h.engine.subscribe();
    loop {
        let tab = h.engine.browser().find_state(board);
        if tab.as_ref().is_some_and(&f) {
            return true;
        }
        if Instant::now() > deadline {
            eprintln!("tab state: {tab:?}");
            return false;
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), events.recv()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_view_frames_input_navigation_popups_and_dialogs() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/")).await;
    let (a, mut a_rx) = h.engine.browser_viewer();
    let (other, mut other_rx) = h.engine.browser_viewer();

    let tab: BrowserTab = serde_json::from_value(
        call(
            &h,
            &a,
            Request::BrowserAttach {
                board_id: b.id.clone(),
                width: 800,
                height: 600,
                dpr: 1.0,
                mobile: false,
                pipelined: false,
                formats: vec![],
            },
        )
        .await,
    )
    .unwrap();
    assert_eq!(tab.board_id, b.id);
    let (n, bytes, w, hgt) = frame(&h, &mut a_rx, &a, &b.id)
        .await
        .expect("a first frame");
    assert!(n >= 1 && bytes > 1000, "frame {n}: {bytes} bytes");
    assert_eq!((w, hgt), (800, 600), "frames report the CSS viewport");
    assert!(title_becomes(&h, &b.id, "Start").await);
    // Only the attached connection gets frames.
    assert!(
        next(&mut other_rx, 1, |e| matches!(
            e,
            Event::BrowserFrame { .. }
        )
        .then_some(()))
        .await
        .is_none(),
        "a connection that didn't attach got a frame"
    );

    // Clicking and typing through input events reaches the page, and the
    // focused field is reported (phones raise their keyboard).
    let click = |x: f64, y: f64| {
        [BrowserMouseKind::Down, BrowserMouseKind::Up].map(|kind| Request::BrowserInput {
            board_id: b.id.clone(),
            event: BrowserInputEvent::Mouse {
                kind,
                x,
                y,
                button: BrowserMouseButton::Left,
                clicks: 1,
                dx: 0.0,
                dy: 0.0,
                modifiers: 0,
            },
        })
    };
    for r in click(50.0, 315.0) {
        call(&h, &a, r).await;
    }
    let focus = next(&mut a_rx, 5, |e| match e {
        Event::BrowserFocus {
            editable,
            input_type,
            ..
        } => Some((*editable, input_type.clone())),
        _ => None,
    })
    .await;
    assert_eq!(focus, Some((true, "text".to_string())));
    for ev in [
        BrowserInputEvent::Text { text: "hé".into() },
        BrowserInputEvent::Key {
            kind: BrowserKeyKind::Down,
            key: "y".into(),
            code: "KeyY".into(),
            text: None,
            modifiers: 0,
        },
        BrowserInputEvent::Key {
            kind: BrowserKeyKind::Up,
            key: "y".into(),
            code: "KeyY".into(),
            text: None,
            modifiers: 0,
        },
        BrowserInputEvent::Key {
            kind: BrowserKeyKind::Down,
            key: "Backspace".into(),
            code: "Backspace".into(),
            text: None,
            modifiers: 0,
        },
        BrowserInputEvent::Text { text: "!".into() },
    ] {
        call(
            &h,
            &a,
            Request::BrowserInput {
                board_id: b.id.clone(),
                event: ev,
            },
        )
        .await;
    }
    assert!(
        title_becomes(&h, &b.id, "typed:hé!").await,
        "text, keys and Backspace"
    );

    // A dialog goes to the attached connection; answering closes it.
    for r in click(450.0, 315.0) {
        call(&h, &a, r).await;
    }
    let dialog = next(&mut a_rx, 5, |e| match e {
        Event::BrowserDialog { kind, message, .. } => Some((*kind, message.clone())),
        _ => None,
    })
    .await;
    assert_eq!(
        dialog,
        Some((BrowserDialogKind::Confirm, "Sure?".to_string()))
    );
    call(
        &h,
        &a,
        Request::BrowserDialogAnswer {
            board_id: b.id.clone(),
            accept: true,
            text: None,
        },
    )
    .await;
    assert!(
        next(&mut a_rx, 5, |e| matches!(
            e,
            Event::BrowserDialogClosed { .. }
        )
        .then_some(()))
        .await
        .is_some()
    );
    assert!(title_becomes(&h, &b.id, "yes").await);

    // An ordinary target=_blank link is now a saved tab, not a sign-in popup.
    // The original page and its history stay untouched.
    for r in click(300.0, 315.0) {
        call(&h, &a, r).await;
    }
    assert!(state_becomes(&h, &b.id, |t| !t.popup && t.tab_ids.len() == 2).await);
    let ids = h.engine.browser().find_state(&b.id).unwrap().tab_ids;
    let linked = ids.iter().find(|id| **id != b.id).unwrap();
    attach(&h, &a, linked, false).await;
    assert!(title_becomes(&h, linked, "Two").await);
    call(
        &h,
        &a,
        Request::BrowserDetach {
            board_id: linked.clone(),
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "yes").await, "opener is unchanged");
    call(&h, &a, Request::DeleteBoard { id: linked.clone() }).await;
    assert!(
        state_becomes(&h, &b.id, |t| !t.popup
            && t.title == "yes"
            && t.tab_ids.len() <= 1)
        .await
    );
    // Return with the same viewport as the initial attachment. The generic
    // attach helper's 1024x700 default would change the later zoom's CSS size.
    call(
        &h,
        &a,
        Request::BrowserAttach {
            board_id: b.id.clone(),
            width: 800,
            height: 600,
            dpr: 1.0,
            mobile: false,
            pipelined: false,
            formats: vec![],
        },
    )
    .await;
    call(
        &h,
        &a,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("{site}/two"),
            },
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Two").await);
    assert!(state_becomes(&h, &b.id, |t| t.url.ends_with("/two") && t.can_go_back).await);
    // The board remembers where its tab is (without a version bump).
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let stored: Whiteboard = serde_json::from_value(
        h.engine
            .handle(Request::GetBoard { id: b.id.clone() }, None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.version, b.version);
    assert!(matches!(&stored.content, BoardContent::Url { url } if url.ends_with("/two")));

    // Back, zoom, and an address typed into the bar.
    let t: BrowserTab = serde_json::from_value(
        call(
            &h,
            &a,
            Request::BrowserNav {
                board_id: b.id.clone(),
                action: BrowserNavAction::Back,
            },
        )
        .await,
    )
    .unwrap();
    assert_eq!(t.board_id, b.id);
    // (Restored as it was: the back/forward cache keeps the page's state.)
    assert!(state_becomes(&h, &b.id, |t| !t.url.ends_with("/two") && t.can_go_forward).await);
    let t: BrowserTab = serde_json::from_value(
        call(
            &h,
            &a,
            Request::BrowserNav {
                board_id: b.id.clone(),
                action: BrowserNavAction::Zoom { factor: 2.0 },
            },
        )
        .await,
    )
    .unwrap();
    assert_eq!(t.zoom, 2.0);
    // Zoomed in, the frames show half the CSS width (same pixel size).
    let mut zoomed = None;
    for _ in 0..10 {
        let f = frame(&h, &mut a_rx, &a, &b.id)
            .await
            .expect("frames after zoom");
        if f.2 == 400 {
            zoomed = Some(f);
            break;
        }
    }
    assert!(zoomed.is_some(), "frames follow the zoom");
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "chrome://settings",
    ] {
        let e = h
            .engine
            .handle_as(
                Request::BrowserNav {
                    board_id: b.id.clone(),
                    action: BrowserNavAction::Go { url: bad.into() },
                },
                None,
                Some(&a),
            )
            .await
            .unwrap_err();
        assert!(
            e.message.contains("can't be opened"),
            "{bad}: {}",
            e.message
        );
    }

    // Detached: no more frames, and the browser stops streaming.
    call(
        &h,
        &a,
        Request::BrowserDetach {
            board_id: b.id.clone(),
        },
    )
    .await;
    while a_rx.try_recv().is_ok() {}
    assert!(h.engine.browser().stats().viewers == 0);

    // Settings: what runs, and a change that applies on the next start.
    let info: BrowserInfo =
        serde_json::from_value(h.engine.handle(Request::GetBrowser, None).await.unwrap()).unwrap();
    assert!(
        info.found && info.running && info.headless && info.tabs >= 1,
        "{info:?}"
    );
    assert!(
        info.name
            .as_deref()
            .is_some_and(|n| n.chars().any(|c| c.is_ascii_digit())),
        "{info:?}"
    );
    let info: BrowserInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::SetBrowser {
                    enabled: None,
                    headless: Some(true),
                    quality: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        !info.running,
        "an unwatched browser restarts with new settings: {info:?}"
    );
    assert!(
        std::fs::read_to_string(h.dir.path().join("config.toml"))
            .unwrap()
            .contains("headless = true")
    );
    drop(other);
    let _ = &mut other_rx;
}

/// Answers approvals as `decide` says and records them.
fn approver(
    h: &Harness,
    decide: impl Fn(&ApprovalRequest) -> ApprovalDecision + Send + 'static,
) -> Arc<Mutex<Vec<ApprovalRequest>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut rx = h.engine.subscribe();
    let engine = h.engine.clone();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = ev
            {
                let decision = decide(&request);
                log.lock().push(request.clone());
                let _ = engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision,
                        },
                        None,
                    )
                    .await;
            }
        }
    });
    seen
}

fn browser_call(args: serde_json::Value) -> Reply {
    Reply::Tools(vec![("browser".into(), args)])
}

async fn secret(h: &Harness, name: &str, value: &str, hosts: &[&str]) {
    h.engine
        .handle(
            Request::SaveSecret {
                name: name.into(),
                kind: SecretKind::Token,
                description: String::new(),
                fields: [("value".to_string(), value.to_string())].into(),
                remove_fields: Vec::new(),
                ask: true,
                hosts: hosts.iter().map(|h| h.to_string()).collect(),
                bots: None,
                on_server: None,
                on_machines: None,
                new_name: None,
                machines_default: None,
            },
            None,
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_reads_types_signs_in_and_downloads_with_approvals() {
    if !have_browser() {
        return;
    }
    let (site, posted) = site().await;
    let url = format!("{site}/");
    let h = harness(
        vec![
            browser_call(serde_json::json!({"action": "open", "url": url})),
            browser_call(serde_json::json!({"action": "type", "ref": "e1", "text": "Ann"})),
            browser_call(serde_json::json!({"action": "type", "ref": "e2", "text": "$ELSEWHERE"})),
            browser_call(serde_json::json!({"action": "type", "ref": "e2", "text": "$SITE_PW"})),
            browser_call(serde_json::json!({"action": "click", "ref": "e3"})),
            Reply::Text("signed in".into()),
            browser_call(serde_json::json!({"action": "navigate", "url": url})),
            browser_call(serde_json::json!({"action": "type", "ref": "e2", "text": "plain"})),
            browser_call(serde_json::json!({"action": "click", "text": "Get file"})),
            browser_call(serde_json::json!({"action": "snapshot", "text": "far"})),
            browser_call(serde_json::json!({"action": "select", "ref": "e9", "text": "green"})),
            browser_call(serde_json::json!({"action": "press", "key": "Control+a"})),
            browser_call(serde_json::json!({"action": "screenshot"})),
            browser_call(serde_json::json!({"action": "tabs"})),
            browser_call(serde_json::json!({"action": "show"})),
            Reply::Text("done".into()),
        ],
        100_000,
    )
    .await;
    secret(&h, "ELSEWHERE", "other-value-123", &["example.com"]).await;
    secret(&h, "SITE_PW", "s3cret-xyz-789", &["127.0.0.1"]).await;
    // Typing a plain password is declined; everything else is allowed.
    let asked = approver(&h, |r| {
        if r.reason.contains("password field") {
            ApprovalDecision::Deny
        } else {
            ApprovalDecision::Allow
        }
    });
    let s = h.session().await;
    h.send(&s.id, "sign in to the test site").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let results = tool_results(&h.entries(&s.id).await);
    let get = |i: usize| results.get(i).cloned().unwrap_or_default();
    let (_, err, open) = get(0);
    assert!(!err, "{open}");
    for want in [
        "Opened background tab",
        "# Test page",
        "[e1] textbox \"Name\" (empty)",
        "[e2] password \"Password\" (empty)",
        "[e3] button \"Sign in\" (submit)",
        "[e9] combobox",
        "— below the visible area —",
    ] {
        assert!(open.contains(want), "{want} missing from:\n{open}");
    }
    assert!(
        get(1)
            .2
            .contains("Typed \"Ann\" into textbox \"Name\" [e1]"),
        "{}",
        get(1).2
    );
    assert!(
        get(2).1 && get(2).2.contains("only for example.com"),
        "{:?}",
        get(2)
    );
    assert!(
        !get(3).1 && get(3).2.contains("Typed $SITE_PW"),
        "{:?}",
        get(3)
    );
    let (_, err, clicked) = get(4);
    assert!(
        !err && clicked.contains("Clicked button \"Sign in\"") && clicked.contains("Signed in"),
        "{clicked}"
    );
    assert_eq!(posted.lock().as_slice(), ["name=Ann&pw=s3cret-xyz-789"]);
    for (_, _, text) in &results {
        assert!(
            !text.contains("s3cret-xyz") && !text.contains("other-value"),
            "a secret leaked: {text}"
        );
    }

    h.send(&s.id, "now get the file").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let results = tool_results(&h.entries(&s.id).await);
    let get = |i: usize| results.get(5 + i).cloned().unwrap_or_default();
    assert!(!get(0).1, "{:?}", get(0));
    assert!(get(1).1 && get(1).2.contains("declined"), "{:?}", get(1));
    let (_, err, got) = get(2);
    assert!(!err && got.contains("Downloaded notes.txt"), "{got}");
    let file = h.dir.path().join("downloads").join("notes.txt");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "file body");
    let (_, err, filtered) = get(3);
    assert!(
        !err && filtered.contains("Far below") && !filtered.contains("Test page\n"),
        "{filtered}"
    );
    assert!(get(4).2.contains("Selected \"Green\""), "{:?}", get(4));
    assert!(!get(5).1, "{:?}", get(5));
    assert!(
        get(6).1 && get(6).2.contains("can't see images"),
        "{:?}",
        get(6)
    );
    assert!(
        get(7).2.contains("* t") && get(7).2.contains("background (yours)"),
        "{:?}",
        get(7)
    ); // `show` moves the background tab, page and all, into a tile.
    let (_, err, shown) = get(8);
    assert!(
        !err && shown.contains("in a tile beside the chat"),
        "{shown}"
    );
    let boards: Vec<Whiteboard> = serde_json::from_value(
        h.engine
            .handle(
                Request::ListBoards {
                    session_id: Some(s.id.clone()),
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let tile = boards
        .iter()
        .find(|b| matches!(&b.content, BoardContent::Url { url } if url.starts_with(&site)))
        .expect("a tile");
    let tab = h
        .engine
        .browser()
        .find_state(&tile.id)
        .expect("the tile has the tab");
    assert_eq!(tab.title, "Start");

    let asked: Vec<String> = asked.lock().iter().map(|r| r.reason.clone()).collect();
    let count = |s: &str| asked.iter().filter(|r| r.contains(s)).count();
    assert_eq!(count("local or private network"), 2, "{asked:#?}");
    assert_eq!(count("stored secret"), 1, "{asked:#?}");
    assert_eq!(count("submits a form"), 1, "{asked:#?}");
    assert_eq!(count("password field"), 1, "{asked:#?}");
    assert_eq!(count("downloads a file"), 1, "{asked:#?}");
    assert_eq!(asked.len(), 6, "{asked:#?}");

    // Private chats don't get the browser (it holds the user's logins).
    let private: SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: None,
                    bot_id: None,
                    private: true,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    h.send(&private.id, "hello").await;
    h.settle(&private.id).await;
    let last = h.mock.agent_requests().last().cloned().unwrap();
    let tools: Vec<String> = last["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(!tools.contains(&"browser".to_string()), "{tools:?}");
    assert!(tools.contains(&"web_fetch".to_string()), "{tools:?}");
}

/// A harness whose config adds `extra` (e.g. a `[browser]` section).
async fn harness_with(extra: &str) -> Harness {
    let mock = Mock::start(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n{extra}\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nname = \"Mock\"\nkind = \"openai_chat\"\nbase_url = \"{}\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 100000\nmax_output = 2000\n",
            mock.url
        ),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

async fn attach(
    h: &Harness,
    v: &tessera_core::browser::Viewer,
    board: &str,
    mobile: bool,
) -> BrowserTab {
    serde_json::from_value(
        call(
            h,
            v,
            Request::BrowserAttach {
                board_id: board.into(),
                width: if mobile { 412 } else { 1024 },
                height: if mobile { 915 } else { 700 },
                dpr: if mobile { 2.625 } else { 1.0 },
                mobile,
                pipelined: false,
                formats: vec![],
            },
        )
        .await,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_apps_crash_recovery_screenshots_and_idle_stop() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let port: u16 = site.rsplit(':').next().unwrap().parse().unwrap();
    let h = harness_with("[browser]\nidle_minutes = 0.05").await;
    let s = h.session().await;
    let b: Whiteboard = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateBoard {
                    session_id: s.id.clone(),
                    title: "App".into(),
                    placed: false,
                    content: BoardContent::Local {
                        port,
                        path: "/two".into(),
                    },
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let browser = h.engine.browser().clone();
    let mut notices = h.engine.subscribe();
    let (v, mut rx) = h.engine.browser_viewer();
    // A phone: mobile layout and a frame sized for its screen.
    attach(&h, &v, &b.id, true).await;
    let (_, _, w, hgt) = frame(&h, &mut rx, &v, &b.id).await.expect("a frame");
    assert_eq!((w, hgt), (412, 915));
    assert!(title_becomes(&h, &b.id, "Two").await);
    assert!(
        h.engine
            .browser()
            .find_state(&b.id)
            .unwrap()
            .url
            .starts_with("http://127.0.0.1:")
    );
    // `whiteboard look` gets the tab's picture from the server itself.
    let shot = browser.board_screenshot(&b.id).await.expect("a screenshot");
    assert!(
        shot.mime == "image/jpeg" && shot.width <= 1600 && shot.height <= 1600,
        "{}×{}",
        shot.width,
        shot.height
    );
    assert_eq!(browser.stats().launches, 1);

    // A tap on a phone focuses the field (and raises its keyboard).
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("{site}/"),
            },
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Start").await);
    for (kind, points) in [
        (
            BrowserTouchKind::Start,
            vec![BrowserTouchPoint {
                x: 50.0,
                y: 315.0,
                id: 0,
            }],
        ),
        (BrowserTouchKind::End, vec![]),
    ] {
        call(
            &h,
            &v,
            Request::BrowserInput {
                board_id: b.id.clone(),
                event: BrowserInputEvent::Touch {
                    kind,
                    points,
                    modifiers: 0,
                },
            },
        )
        .await;
    }
    let focus = next(&mut rx, 5, |e| match e {
        Event::BrowserFocus { editable, .. } => Some(*editable),
        _ => None,
    })
    .await;
    assert_eq!(focus, Some(true), "a tap focuses the field");
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Back,
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Two").await);

    // The browser dies: it comes back, at the same page, for the viewer.
    let pid = browser.pid().await.expect("running");
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    let warned = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Notice { message, .. }) = notices.recv().await
                && message.contains("stopped unexpectedly")
            {
                return;
            }
        }
    })
    .await;
    assert!(warned.is_ok(), "the user was told");
    let mut back = false;
    for _ in 0..20 {
        if frame(&h, &mut rx, &v, &b.id).await.is_some() && browser.stats().launches == 2 {
            back = true;
            break;
        }
    }
    assert!(
        back,
        "frames resumed after a restart: {:?}",
        browser.stats()
    );
    assert!(title_becomes(&h, &b.id, "Two").await);
    assert_ne!(browser.pid().await, Some(pid));

    // Nobody watching: it stops after the idle time, and comes back on use.
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: b.id.clone(),
        },
    )
    .await;
    let stopped = tokio::time::timeout(Duration::from_secs(15), async {
        while browser.stats().running {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(stopped.is_ok(), "stopped when idle");
    while rx.try_recv().is_ok() {}
    attach(&h, &v, &b.id, false).await;
    assert!(frame(&h, &mut rx, &v, &b.id).await.is_some());
    assert!(
        title_becomes(&h, &b.id, "Two").await,
        "restored from the board's address"
    );
    assert_eq!(browser.stats().launches, 3);
    // The profile is private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(h.dir.path().join("browser"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    // Deleting the board closes its tab.
    h.engine
        .handle(Request::DeleteBoard { id: b.id.clone() }, None)
        .await
        .unwrap();
    assert!(browser.find_state(&b.id).is_none());
}

/// CPU seconds and proportional memory (MB) of the browser's processes.
#[cfg(target_os = "linux")]
fn tree_use(pgid: u32) -> (f64, f64, usize) {
    let mut cpu = 0.0;
    let mut pss = 0.0;
    let mut n = 0;
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
        if f.get(2).and_then(|g| g.parse::<u32>().ok()) != Some(pgid) {
            continue;
        }
        n += 1;
        cpu += (f[11].parse::<f64>().unwrap_or(0.0) + f[12].parse::<f64>().unwrap_or(0.0)) / 100.0;
        if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) {
            for l in s.lines() {
                if let Some(v) = l.strip_prefix("Pss:") {
                    pss += v
                        .trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                        / 1024.0;
                }
            }
        }
    }
    (cpu, pss, n)
}

#[cfg(target_os = "linux")]
fn own_cpu() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let rest = stat.rsplit_once(')').unwrap().1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

/// Stream for `secs`, acknowledging each frame after `delay`: (frames, bytes).
#[cfg(target_os = "linux")]
async fn stream(
    h: &Harness,
    v: &tessera_core::browser::Viewer,
    rx: &mut mpsc::Receiver<Event>,
    board: &str,
    secs: f64,
    delay: Duration,
) -> (u64, u64) {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let (mut n, mut bytes) = (0, 0);
    while Instant::now() < end {
        let Ok(Some(ev)) = tokio::time::timeout_at(end.into(), rx.recv()).await else {
            break;
        };
        if let Event::BrowserFrame { frame, data, .. } = ev {
            n += 1;
            bytes += (data.len() * 3 / 4) as u64;
            tokio::time::sleep(delay).await;
            call(
                h,
                v,
                Request::BrowserAck {
                    board_id: board.into(),
                    frame,
                },
            )
            .await;
        }
    }
    (n, bytes)
}

/// Run with `cargo test -p tessera-core --test browser measure -- --ignored --nocapture`.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn measure_resource_use() {
    if !have_browser() {
        return;
    }
    let ticker = Router::new()
        .route("/", get(|| async { axum::response::Html("<!doctype html><title>Ticker</title><style>body{font:40px sans-serif}</style><h1 id=t>0</h1><p>A counter that changes every 33 ms, with some text around it so the frame has detail.</p><script>let n=0;setInterval(()=>{document.getElementById('t').textContent=++n},33)</script>") }))
        .route("/static", get(|| async { axum::response::Html(START) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, ticker).await.unwrap() });
    let h = harness_with("[browser]\nfreeze_seconds = 4").await;
    let s = h.session().await;
    let browser = h.engine.browser().clone();
    let stat = board(&h, &s.id, &format!("http://{addr}/static")).await;
    let tick = board(&h, &s.id, &format!("http://{addr}/")).await;
    let (v, mut rx) = h.engine.browser_viewer();

    let t0 = Instant::now();
    attach(&h, &v, &stat.id, false).await;
    frame(&h, &mut rx, &v, &stat.id).await.unwrap();
    println!(
        "start + first frame: {:.0} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    let pid = browser.pid().await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let measure = |label: &'static str,
                   frames: u64,
                   bytes: u64,
                   secs: f64,
                   before: (f64, f64, usize),
                   me: f64| {
        let after = tree_use(pid);
        println!(
            "{label}: {frames} frames in {secs:.0} s ({:.1} fps), {:.1} KB/frame, {:.0} KB/s; browser CPU {:.1}% ({} processes, {:.0} MB PSS); server CPU {:.1}%",
            frames as f64 / secs,
            if frames > 0 {
                bytes as f64 / frames as f64 / 1024.0
            } else {
                0.0
            },
            bytes as f64 / secs / 1024.0,
            (after.0 - before.0) / secs * 100.0,
            after.2,
            after.1,
            (own_cpu() - me) / secs * 100.0,
        );
    };

    // A static page with a viewer attached: nothing to send.
    let (before, me) = (tree_use(pid), own_cpu());
    let (n, b) = stream(&h, &v, &mut rx, &stat.id, 5.0, Duration::ZERO).await;
    measure("static page, 1024×700 viewer", n, b, 5.0, before, me);

    // Nobody watching (detached), page loaded: idle.
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: stat.id.clone(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (before, me) = (tree_use(pid), own_cpu());
    tokio::time::sleep(Duration::from_secs(5)).await;
    measure("idle, 1 tab, no viewer", 0, 0, 5.0, before, me);

    // An animated page streamed to a fast desktop viewer.
    attach(&h, &v, &tick.id, false).await;
    let (before, me) = (tree_use(pid), own_cpu());
    let (n, b) = stream(&h, &v, &mut rx, &tick.id, 5.0, Duration::ZERO).await;
    measure(
        "animated page, 1024×700 @1x, fast viewer",
        n,
        b,
        5.0,
        before,
        me,
    );

    // The same for a phone (412×915 CSS at 2.625x).
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: tick.id.clone(),
        },
    )
    .await;
    while rx.try_recv().is_ok() {}
    attach(&h, &v, &tick.id, true).await;
    let (before, me) = (tree_use(pid), own_cpu());
    let (n, b) = stream(&h, &v, &mut rx, &tick.id, 5.0, Duration::ZERO).await;
    measure(
        "animated page, phone 412×915 @2.625x, fast viewer",
        n,
        b,
        5.0,
        before,
        me,
    );

    // A slow link (300 ms per frame): fewer frames, lower quality.
    let (before, me) = (tree_use(pid), own_cpu());
    let (n, b) = stream(&h, &v, &mut rx, &tick.id, 8.0, Duration::from_millis(300)).await;
    measure(
        "animated page, phone, 300 ms acks (adapting)",
        n,
        b,
        8.0,
        before,
        me,
    );
    let (before, me) = (tree_use(pid), own_cpu());
    let (n, b) = stream(&h, &v, &mut rx, &tick.id, 5.0, Duration::from_millis(300)).await;
    measure(
        "animated page, phone, 300 ms acks (adapted)",
        n,
        b,
        5.0,
        before,
        me,
    );

    // A viewer that stops acknowledging: the browser stops painting for it.
    let (before, me) = (tree_use(pid), own_cpu());
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut unacked = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, Event::BrowserFrame { .. }) {
            unacked += 1;
        }
    }
    measure(
        "animated page, viewer not acknowledging",
        unacked,
        0,
        5.0,
        before,
        me,
    );

    // Nobody watching an animated page: it runs until frozen, then rests.
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: tick.id.clone(),
        },
    )
    .await;
    let (before, me) = (tree_use(pid), own_cpu());
    tokio::time::sleep(Duration::from_secs(3)).await;
    measure(
        "animated page, no viewer (before freezing)",
        0,
        0,
        3.0,
        before,
        me,
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (before, me) = (tree_use(pid), own_cpu());
    tokio::time::sleep(Duration::from_secs(5)).await;
    measure("animated page, no viewer, frozen", 0, 0, 5.0, before, me);
    // Looking again thaws it.
    while rx.try_recv().is_ok() {}
    attach(&h, &v, &tick.id, false).await;
    let (n, _) = stream(&h, &v, &mut rx, &tick.id, 2.0, Duration::ZERO).await;
    assert!(n > 10, "a thawed page streams again ({n} frames)");
    println!("stats: {:?}", browser.stats());
}

/// A real model using the tool (ignored by default):
///   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test browser live -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn live_model_uses_the_browser() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        eprintln!("set TESSERA_LIVE_MODEL");
        return;
    };
    if !have_browser() {
        return;
    }
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').unwrap();
    let (site, _) = site().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
        ),
    )
    .unwrap();
    let engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let h = Harness {
        engine,
        mock: Mock::start(vec![]).await,
        dir,
        work: std::env::temp_dir(),
        _work_dir: tempfile::tempdir().unwrap(),
    };
    let asked = approver(&h, |_| ApprovalDecision::Allow);
    let s = h.session().await;
    let t0 = Instant::now();
    h.send(
        &s.id,
        &format!("Use the browser: open {site}/ , type hello into its Search box, pick Green in the list, then tell me the page's main heading. Be brief."),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(600), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let entries = h.entries(&s.id).await;
    for e in &entries {
        match &e.body {
            EntryBody::Assistant { content, .. } => {
                for p in content {
                    match p {
                        AssistantPart::Text { text } => eprintln!("[assistant] {text}"),
                        AssistantPart::ToolCall {
                            name, arguments, ..
                        } => eprintln!("[call] {name} {arguments}"),
                        _ => {}
                    }
                }
            }
            EntryBody::ToolResult {
                content, is_error, ..
            } => {
                eprintln!(
                    "[result{}] {}",
                    if *is_error { " ERROR" } else { "" },
                    tessera_core::util::truncate(&plain_text(content), 400)
                )
            }
            _ => {}
        }
    }
    eprintln!(
        "{:.0} s, approvals: {:?}",
        t0.elapsed().as_secs_f64(),
        asked
            .lock()
            .iter()
            .map(|r| r.reason.clone())
            .collect::<Vec<_>>()
    );
    let calls = tool_results(&entries);
    assert!(calls.iter().any(|(t, err, _)| t == "browser" && !err));
}

async fn click_at(h: &Harness, v: &tessera_core::browser::Viewer, board: &str, x: f64, y: f64) {
    for kind in [BrowserMouseKind::Down, BrowserMouseKind::Up] {
        call(
            h,
            v,
            Request::BrowserInput {
                board_id: board.into(),
                event: BrowserInputEvent::Mouse {
                    kind,
                    x,
                    y,
                    button: BrowserMouseButton::Left,
                    clicks: 1,
                    dx: 0.0,
                    dy: 0.0,
                    modifiers: 0,
                },
            },
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_popups_post_back_to_their_opener_and_redirect_logins_persist() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/login")).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("a frame");
    assert!(title_becomes(&h, &b.id, "Login").await);

    // "Sign in with Provider" opens a popup: the tile shows it on top.
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.popup && t.url.contains("/oauth/authorize")).await,
        "the sign-in window is on top"
    );
    let tab = h.engine.browser().find_state(&b.id).unwrap();
    assert!(
        tab.opener.as_deref().is_some_and(|o| o.ends_with("/login")),
        "{tab:?}"
    );
    assert!(tab.can_go_back, "Back closes a popup");
    // Its frames come to the same tile.
    while rx.try_recv().is_ok() {}
    let mut popup_frames = 0;
    for _ in 0..3 {
        if frame(&h, &mut rx, &v, &b.id).await.is_some() {
            popup_frames += 1;
            break;
        }
    }
    assert!(popup_frames > 0);
    // Allow: the provider redirects to its callback, which posts the code
    // to the opener and closes itself; the page under it is back on top.
    assert!(title_becomes(&h, &b.id, "Provider").await);
    // Chrome ignores clicks on a page it hasn't painted yet: click once it
    // is on screen, as a person would (a busy server paints late). A page
    // that painted already sends nothing new, hence no frame is required.
    let _ = tokio::time::timeout(Duration::from_secs(2), frame(&h, &mut rx, &v, &b.id)).await;
    let done = |t: &BrowserTab| !t.popup && t.title == "logged in: xyz";
    click_at(&h, &v, &b.id, 40.0, 315.0).await;
    // A loaded machine may still drop that first click: again, while the
    // sign-in window is still the one on top.
    for _ in 0..2 {
        if state_within(&h, &b.id, 4, done).await {
            break;
        }
        if state_within(&h, &b.id, 1, |t| t.popup && t.title == "Provider").await {
            click_at(&h, &v, &b.id, 40.0, 315.0).await;
        }
    }
    assert!(
        state_becomes(&h, &b.id, done).await,
        "the opener got the code and the popup closed"
    );
    // The tile streams the opener again.
    while rx.try_recv().is_ok() {}
    assert!(frame(&h, &mut rx, &v, &b.id).await.is_some());

    // Back closes a popup without touching the page under it.
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.popup).await);
    let t: BrowserTab = serde_json::from_value(
        call(
            &h,
            &v,
            Request::BrowserNav {
                board_id: b.id.clone(),
                action: BrowserNavAction::Back,
            },
        )
        .await,
    )
    .unwrap();
    assert!(!t.popup && t.title == "logged in: xyz", "{t:?}");

    // A redirect sign-in in the tab itself sets a lasting session...
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("{site}/oauth/authorize?mode=redirect"),
            },
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Provider").await);
    // Chrome ignores clicks on a page it hasn't painted yet: click once it
    // is on screen, as a person would (a busy server paints late).
    while rx.try_recv().is_ok() {}
    assert!(frame(&h, &mut rx, &v, &b.id).await.is_some());
    click_at(&h, &v, &b.id, 40.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.title == "Account"
            && t.url.ends_with("/account"))
        .await
    );
    // ...that survives the browser stopping (the profile keeps logins).
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: b.id.clone(),
        },
    )
    .await;
    let info: BrowserInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::SetBrowser {
                    enabled: None,
                    headless: Some(true),
                    quality: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!info.running);
    attach(&h, &v, &b.id, false).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.title == "Account").await,
        "still signed in after a restart"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_signs_in_through_a_popup() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness(
        vec![
            browser_call(serde_json::json!({"action": "open", "url": format!("{site}/login")})),
            browser_call(serde_json::json!({"action": "click", "text": "Sign in with Provider"})),
            browser_call(serde_json::json!({"action": "tabs"})),
            browser_call(serde_json::json!({"action": "click", "text": "Allow"})),
            browser_call(serde_json::json!({"action": "click", "text": "Sign in with Provider"})),
            browser_call(serde_json::json!({"action": "back"})),
            Reply::Text("signed in".into()),
        ],
        100_000,
    )
    .await;
    let asked = approver(&h, |_| ApprovalDecision::Allow);
    let s = h.session().await;
    h.send(&s.id, "sign in with the provider").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let r = tool_results(&h.entries(&s.id).await);
    let get = |i: usize| r.get(i).cloned().unwrap_or_default();
    assert!(!get(0).1 && get(0).2.contains("Signed out"), "{:?}", get(0));
    let (_, err, popup) = get(1);
    assert!(
        !err && popup.contains("A popup window opened")
            && popup.contains("[a popup window over")
            && popup.contains("button \"Allow\""),
        "{popup}"
    );
    // Its very first request already looked like an ordinary Chrome.
    assert!(
        popup.contains("UA: Mozilla/5.0") && popup.contains("Chrome/"),
        "{popup}"
    );
    assert!(!popup.contains("HeadlessChrome"), "{popup}");
    assert!(get(2).2.contains("popup open on top"), "{:?}", get(2));
    let (_, err, done) = get(3);
    assert!(
        !err && done.contains("The popup closed") && done.contains("Logged in as Ann"),
        "{done}"
    );
    assert!(get(4).2.contains("A popup window opened"), "{:?}", get(4));
    let (_, err, back) = get(5);
    assert!(
        !err && back.contains("Closed the popup") && back.contains("Logged in as Ann"),
        "{back}"
    );
    let asked: Vec<String> = asked.lock().iter().map(|a| a.reason.clone()).collect();
    assert!(
        asked.iter().any(|a| a.contains("submits a form")),
        "{asked:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_are_sharp_on_dense_screens() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    // Sharp: whatever this machine's measured speed.
    let h = harness_with("[browser]\nquality = \"sharp\"").await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/two")).await;
    let (v, mut rx) = h.engine.browser_viewer();
    for (w, ht, dpr, mobile, want) in [
        (375, 738, 2.0, true, (750, 1476)),
        (800, 600, 1.0, false, (800, 600)),
        (1280, 800, 2.0, false, (2048, 1280)),
    ] {
        call(
            &h,
            &v,
            Request::BrowserAttach {
                board_id: b.id.clone(),
                width: w,
                height: ht,
                dpr,
                mobile,
                pipelined: false,
                formats: vec![],
            },
        )
        .await;
        let mut got = None;
        for _ in 0..10 {
            let ev = next(&mut rx, 10, |e| match e {
                Event::BrowserFrame {
                    frame,
                    data,
                    width,
                    height,
                    ..
                } => Some((*frame, data.clone(), *width, *height)),
                _ => None,
            })
            .await
            .expect("a frame");
            call(
                &h,
                &v,
                Request::BrowserAck {
                    board_id: b.id.clone(),
                    frame: ev.0,
                },
            )
            .await;
            let px = jpeg_size(&ev.1);
            if (ev.2, ev.3) == (w, ht) && px == want {
                got = Some(px);
                break;
            }
            eprintln!("{w}×{ht}@{dpr}: frame says {}×{}, JPEG {px:?}", ev.2, ev.3);
        }
        assert_eq!(got, Some(want), "{w}×{ht} at {dpr}x");
    }
}

/// Pixel size of a base64 WebP (from its first chunk).
fn webp_size(b64: &str) -> (u32, u32) {
    use base64::Engine as _;
    let b = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    assert!(b.len() > 30 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP");
    match &b[12..16] {
        b"VP8X" => (
            1 + u32::from_le_bytes([b[24], b[25], b[26], 0]),
            1 + u32::from_le_bytes([b[27], b[28], b[29], 0]),
        ),
        b"VP8 " => (
            u16::from_le_bytes([b[26], b[27]]) as u32 & 0x3fff,
            u16::from_le_bytes([b[28], b[29]]) as u32 & 0x3fff,
        ),
        _ => (0, 0),
    }
}

/// A frame for `board`: (number, mime, data, width, height, scroll_y).
async fn any_frame(
    rx: &mut mpsc::Receiver<Event>,
    board: &str,
    secs: u64,
) -> Option<(u64, String, String, u32, u32, Option<f64>)> {
    next(rx, secs, |e| match e {
        Event::BrowserFrame {
            board_id,
            frame,
            mime,
            data,
            width,
            height,
            scroll_y,
            ..
        } if board_id == board => Some((
            *frame,
            mime.clone(),
            data.clone(),
            *width,
            *height,
            *scroll_y,
        )),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_frames_and_sharp_still_pictures() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let ticker = Router::new().route(
        "/",
        get(|| async {
            axum::response::Html("<!doctype html><title>Ticker</title><h1 id=t>0</h1><script>let n=0;setInterval(()=>{document.getElementById('t').textContent=++n},20)</script>")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, ticker).await.unwrap() });
    // A fast server (stills are WebP), whatever this machine measures now.
    let h = harness_with("[browser]\nquality = \"sharp\"\nspeed = 60").await;
    let s = h.session().await;
    let page = board(&h, &s.id, &format!("{site}/")).await;
    let tick = board(&h, &s.id, &format!("http://{addr}/")).await;
    let phone = |board: &str, formats: Vec<String>| Request::BrowserAttach {
        board_id: board.into(),
        width: 412,
        height: 915,
        dpr: 2.625,
        mobile: true,
        pipelined: true,
        formats,
    };
    let ack = |board: &str, frame: u64| Request::BrowserAck {
        board_id: board.into(),
        frame,
    };
    let (v, mut rx) = h.engine.browser_viewer();

    // A page that stops changing ends on a sharp WebP at the phone's own
    // pixel ratio (frames are JPEG at the browser's scale, 2).
    call(&h, &v, phone(&page.id, vec!["image/webp".into()])).await;
    let mut still = None;
    for _ in 0..30 {
        let f = any_frame(&mut rx, &page.id, 10).await.expect("a frame");
        assert_eq!((f.3, f.4), (412, 915), "frames say the CSS viewport");
        assert_eq!(f.5, Some(0.0), "frames say where the page is scrolled to");
        call(&h, &v, ack(&page.id, f.0)).await;
        if f.1 == "image/webp" {
            still = Some(f);
            break;
        }
        assert_eq!(f.1, "image/jpeg");
        assert!(jpeg_size(&f.2).0 <= 824, "{:?}", jpeg_size(&f.2));
    }
    let still = still.expect("a still picture once the page rests");
    let (w, ht) = webp_size(&still.2);
    assert!(w.abs_diff(1082) <= 1 && ht.abs_diff(2402) <= 1, "{w}×{ht}");

    // A finger scrolls it: frames show the new position, then a new still.
    let finger = |kind, y: f64| Request::BrowserInput {
        board_id: page.id.clone(),
        event: BrowserInputEvent::Touch {
            kind,
            points: if kind == BrowserTouchKind::End {
                vec![]
            } else {
                vec![BrowserTouchPoint { x: 200.0, y, id: 1 }]
            },
            modifiers: 0,
        },
    };
    call(&h, &v, finger(BrowserTouchKind::Start, 700.0)).await;
    for i in 1..=20 {
        call(
            &h,
            &v,
            finger(BrowserTouchKind::Move, 700.0 - 20.0 * i as f64),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    call(&h, &v, finger(BrowserTouchKind::End, 0.0)).await;
    let (mut scrolled, mut again) = (false, false);
    for _ in 0..60 {
        let Some(f) = any_frame(&mut rx, &page.id, 10).await else {
            break;
        };
        call(&h, &v, ack(&page.id, f.0)).await;
        scrolled |= f.1 == "image/jpeg" && f.5.is_some_and(|y| y > 100.0);
        if scrolled && f.1 == "image/webp" {
            again = true;
            break;
        }
    }
    assert!(scrolled, "frames follow the scroll");
    assert!(again, "a new still picture once it rests again");

    // A connection that draws only JPEG never gets WebP.
    let (v2, mut rx2) = h.engine.browser_viewer();
    call(&h, &v2, phone(&page.id, vec![])).await;
    let t0 = Instant::now();
    let mut got = 0;
    while t0.elapsed() < Duration::from_secs(3) {
        let Some(f) = any_frame(&mut rx2, &page.id, 3).await else {
            break;
        };
        assert_eq!(f.1, "image/jpeg");
        call(&h, &v2, ack(&page.id, f.0)).await;
        got += 1;
    }
    assert!(got >= 1);
    drop(v2);

    // On a page that keeps changing, a pipelined connection gets several
    // frames before acknowledging, but not without end.
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: page.id.clone(),
        },
    )
    .await;
    while rx.try_recv().is_ok() {}
    call(&h, &v, phone(&tick.id, vec!["image/webp".into()])).await;
    // Wait for producer readiness and acknowledge moving (JPEG) frames,
    // not a cached still or the first frame sent while metrics settle.
    assert!(title_becomes(&h, &tick.id, "Ticker").await);
    for _ in 0..12 {
        let f = any_frame(&mut rx, &tick.id, 10)
            .await
            .expect("ticker paints while warming");
        call(&h, &v, ack(&tick.id, f.0)).await;
    }
    let before = h.engine.browser().stats();
    let mut unacked = 0;
    while any_frame(&mut rx, &tick.id, 2).await.is_some() {
        unacked += 1;
        assert!(unacked <= 8, "a window has an end");
    }
    eprintln!(
        "pipeline producer before={before:?} after={:?}, transported unacked={unacked}, max window=8",
        h.engine.browser().stats()
    );
    assert!(
        unacked >= 2,
        "several frames on their way at once ({unacked})"
    );
}

/// The next frame of `board` (acknowledged): (mime, picture size, CSS size).
async fn sized_frame(
    h: &Harness,
    rx: &mut mpsc::Receiver<Event>,
    v: &tessera_core::browser::Viewer,
    board: &str,
) -> Option<(String, (u32, u32), (u32, u32))> {
    let f = any_frame(rx, board, 10).await?;
    call(
        h,
        v,
        Request::BrowserAck {
            board_id: board.into(),
            frame: f.0,
        },
    )
    .await;
    let px = if f.1 == "image/jpeg" {
        jpeg_size(&f.2)
    } else {
        webp_size(&f.2)
    };
    Some((f.1, px, (f.3, f.4)))
}

/// Wait until a JPEG frame of `board` is `want` pixels (the page moves, so
/// frames keep coming).
async fn frames_become(
    h: &Harness,
    rx: &mut mpsc::Receiver<Event>,
    v: &tessera_core::browser::Viewer,
    board: &str,
    want: (u32, u32),
) -> bool {
    let mut seen = Vec::new();
    for _ in 0..40 {
        let Some((mime, px, _)) = sized_frame(h, rx, v, board).await else {
            break;
        };
        if mime == "image/jpeg" && px.0.abs_diff(want.0) <= 1 && px.1.abs_diff(want.1) <= 1 {
            return true;
        }
        seen.push(px);
    }
    eprintln!("wanted {want:?}, frames were {seen:?}");
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn odd_tiles_at_fractional_render_scales_keep_delivering_after_zoom() {
    assert!(have_browser(), "this regression requires real Chrome");
    let ticker = Router::new().route(
        "/",
        get(|| async {
            axum::response::Html("<!doctype html><title>Odd ticker</title><h1 id=t>0</h1><script>let n=0;setInterval(()=>{document.getElementById('t').textContent=++n},30)</script>")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, ticker).await.unwrap() });
    // Use the existing Smooth/speed settings to pin the sub-1x render steps,
    // rather than introduce a test-only override of the browser's geometry.
    for (speed, scale) in [(3, 0.5_f64), (6, 0.75_f64)] {
        let h = harness_with(&format!("[browser]\nquality = \"smooth\"\nspeed = {speed}")).await;
        let s = h.session().await;
        let b = board(&h, &s.id, &format!("http://{addr}/")).await;
        let (v, mut rx) = h.engine.browser_viewer();
        call(
            &h,
            &v,
            Request::BrowserAttach {
                board_id: b.id.clone(),
                width: 501,
                height: 701,
                dpr: 1.0,
                mobile: false,
                pipelined: false,
                formats: vec![],
            },
        )
        .await;
        for zoom in [1.0, 1.1, 1.0] {
            call(
                &h,
                &v,
                Request::BrowserNav {
                    board_id: b.id.clone(),
                    action: BrowserNavAction::Zoom { factor: zoom },
                },
            )
            .await;
            let css = (
                (501.0_f64 / zoom).round() as u32,
                (701.0_f64 / zoom).round() as u32,
            );
            let pixels = (
                (css.0 as f64 * scale).round() as u32,
                (css.1 as f64 * scale).round() as u32,
            );
            // Chrome can initially encode only part of the newly emulated
            // viewport. Require it fully painted before checking continuity,
            // keeping both readiness and all six frames within one deadline.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            assert!(
                tokio::time::timeout_at(deadline, frames_become(&h, &mut rx, &v, &b.id, pixels))
                    .await
                    .unwrap_or(false),
                "odd tile must finish painting at its forced render scale"
            );
            // Repeated fresh, acknowledged frames prove it did not merely
            // deliver one cached picture and then freeze at the rounded size.
            for _ in 0..6 {
                let (mime, px, dip) =
                    tokio::time::timeout_at(deadline, sized_frame(&h, &mut rx, &v, &b.id))
                        .await
                        .expect("odd tile continuity deadline")
                        .expect("odd tile must keep painting at its fractional render scale");
                assert_eq!(mime, "image/jpeg");
                assert!(
                    px.0.abs_diff(pixels.0) <= 1 && px.1.abs_diff(pixels.1) <= 1,
                    "forced {scale}x pixels {px:?}, expected {pixels:?}"
                );
                assert!(
                    dip.0.abs_diff(css.0) <= 1 && dip.1.abs_diff(css.1) <= 1,
                    "post-zoom geometry {dip:?}, expected {css:?}"
                );
            }
        }
        h.engine.shutdown().await;
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_device_gets_frames_drawn_at_its_own_scale() {
    if !have_browser() {
        return;
    }
    // A page that never stops moving (frames keep coming).
    let ticker = Router::new().route(
        "/",
        get(|| async {
            axum::response::Html("<!doctype html><title>Ticker</title><h1 id=t>0</h1><script>let n=0;setInterval(()=>{document.getElementById('t').textContent=++n},30)</script>")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, ticker).await.unwrap() });
    let h = harness_with("[browser]\nquality = \"sharp\"").await;
    let s = h.session().await;
    let desk = board(&h, &s.id, &format!("http://{addr}/")).await;
    let phone = board(&h, &s.id, &format!("http://{addr}/?phone")).await;
    let attach = |board: &str, w: u32, ht: u32, dpr: f64, mobile: bool| Request::BrowserAttach {
        board_id: board.into(),
        width: w,
        height: ht,
        dpr,
        mobile,
        pipelined: false,
        formats: vec![],
    };
    let (vd, mut rxd) = h.engine.browser_viewer();
    let (vp, mut rxp) = h.engine.browser_viewer();
    call(&h, &vd, attach(&desk.id, 800, 600, 1.0, false)).await;
    call(&h, &vp, attach(&phone.id, 412, 915, 2.625, true)).await;

    // At the same time: the desktop's page drawn at 1x, the phone's at 2x
    // (the sharpest `[browser] scale` allows).
    assert!(frames_become(&h, &mut rxd, &vd, &desk.id, (800, 600)).await);
    assert!(frames_become(&h, &mut rxp, &vp, &phone.id, (824, 1830)).await);
    let launches = h.engine.browser().stats().launches;

    // The server's speed was measured when the browser started, and is
    // remembered for its next start.
    let info: BrowserInfo =
        serde_json::from_value(h.engine.handle(Request::GetBrowser, None).await.unwrap()).unwrap();
    assert_eq!(info.quality, BrowserQuality::Sharp);
    let speed = info.speed.expect("measured");
    assert!(speed > 1.0 && speed <= 60.0, "{speed}");
    assert!(
        h.dir.path().join("browser/tessera-speed.json").is_file(),
        "remembered"
    );

    // Smooth: the phone's page is drawn at 1x at once, without a restart.
    let set = |q| Request::SetBrowser {
        enabled: None,
        headless: None,
        quality: Some(q),
    };
    let info: BrowserInfo = serde_json::from_value(
        h.engine
            .handle(set(BrowserQuality::Smooth), None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.quality, BrowserQuality::Smooth);
    // Smooth can be below 1x on slow hardware. Derive the expected scale
    // from the speed actually measured, keeping the sub-1x assertion.
    let smooth = |w: u32, ht: u32| {
        let k = 1000.0 / (speed * 4.096);
        let fit = (10.0 / (k * (w as f64 * ht as f64 / 1e6).max(0.01))).sqrt();
        let scale = if k <= 4.6 || fit >= 1.0 {
            1.0
        } else if fit >= 0.75 {
            0.75
        } else {
            0.5
        };
        (
            (w as f64 * scale).round() as u32,
            (ht as f64 * scale).round() as u32,
        )
    };
    assert!(frames_become(&h, &mut rxp, &vp, &phone.id, smooth(412, 915)).await);
    assert!(frames_become(&h, &mut rxd, &vd, &desk.id, smooth(800, 600)).await);
    assert!(
        std::fs::read_to_string(h.dir.path().join("config.toml"))
            .unwrap()
            .contains("quality = \"smooth\""),
        "saved"
    );
    // Sharp again; and a zoomed desktop tile (200%) is drawn at 2x too.
    h.engine
        .handle(set(BrowserQuality::Sharp), None)
        .await
        .unwrap();
    assert!(frames_become(&h, &mut rxp, &vp, &phone.id, (824, 1830)).await);
    call(
        &h,
        &vd,
        Request::BrowserNav {
            board_id: desk.id.clone(),
            action: BrowserNavAction::Zoom { factor: 2.0 },
        },
    )
    .await;
    assert!(frames_become(&h, &mut rxd, &vd, &desk.id, (800, 600)).await);
    assert_eq!(h.engine.browser().stats().launches, launches, "no restart");
}

/// Web views have no security keys or passkeys: a sign-in page sees a
/// browser without them, a request for one is turned down at once (rather
/// than waiting for a key nobody can plug in), and the password form works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_pages_see_no_security_keys_and_passwords_still_work() {
    if !have_browser() {
        return;
    }
    let (site, posted) = site().await;
    // Security keys need a domain name (not an IP address) to be asked at all.
    let site = site.replace("127.0.0.1", "localhost");
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/keys")).await;
    let (v, mut rx) = h.engine.browser_viewer();
    call(
        &h,
        &v,
        Request::BrowserAttach {
            board_id: b.id.clone(),
            width: 800,
            height: 600,
            dpr: 1.0,
            mobile: false,
            pipelined: false,
            formats: vec![],
        },
    )
    .await;
    frame(&h, &mut rx, &v, &b.id).await.expect("a frame");
    assert!(
        state_within(&h, &b.id, 15, |t| t.title.starts_with("offer:")).await,
        "the page finished asking"
    );
    let title = h.engine.browser().find_state(&b.id).unwrap().title;
    assert_eq!(
        title, "offer:password get:NotAllowedError create:NotAllowedError at-once",
        "no security key offered, and requests for one turned down at once"
    );

    // The ordinary sign-in form: type a name and password, press the button.
    let typing = |text: &str| Request::BrowserInput {
        board_id: b.id.clone(),
        event: BrowserInputEvent::Text { text: text.into() },
    };
    click_at(&h, &v, &b.id, 50.0, 35.0).await;
    call(&h, &v, typing("ann")).await;
    click_at(&h, &v, &b.id, 50.0, 85.0).await;
    call(&h, &v, typing("pa55word")).await;
    click_at(&h, &v, &b.id, 50.0, 135.0).await;
    assert!(title_becomes(&h, &b.id, "Done").await, "the form was sent");
    assert_eq!(
        posted.lock().last().map(String::as_str),
        Some("name=ann&pw=pa55word")
    );
}

/// A page that wants a "prove you're a person" check is noticed (the tile
/// then offers to finish it elsewhere); other pages aren't, and leaving the
/// page clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sites_verification_check_is_noticed_for_the_tile() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/challenge")).await;
    let (v, mut rx) = h.engine.browser_viewer();
    call(
        &h,
        &v,
        Request::BrowserAttach {
            board_id: b.id.clone(),
            width: 800,
            height: 600,
            dpr: 1.0,
            mobile: false,
            pipelined: false,
            formats: vec![],
        },
    )
    .await;
    frame(&h, &mut rx, &v, &b.id).await.expect("a frame");
    assert!(
        state_within(&h, &b.id, 15, |t| t.check).await,
        "the check was noticed"
    );
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("{site}/two"),
            },
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Two").await);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let t = h.engine.browser().find_state(&b.id).unwrap();
    assert!(!t.check, "an ordinary page isn't one: {t:?}");

    // A page that asks only after a sign-in attempt (no new page): noticed
    // after the click, long after it loaded.
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Go {
                url: format!("{site}/challenge-later"),
            },
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "Sign in later").await);
    tokio::time::sleep(Duration::from_millis(6500)).await;
    let t = h.engine.browser().find_state(&b.id).unwrap();
    assert!(!t.check, "nothing asked yet: {t:?}");
    click_at(&h, &v, &b.id, 50.0, 40.0).await;
    assert!(
        state_within(&h, &b.id, 10, |t| t.check).await,
        "the check asked for after the attempt was noticed"
    );
}

/// Controls whose clicks change something, or nothing, and a canvas that
/// keeps where it was clicked (drawn, not in the page's elements).
const CLICKS: &str = r#"<!doctype html><title>Clicks</title>
<style>body{margin:0;font:16px sans-serif}canvas{position:absolute;left:40px;top:50px;width:800px;height:400px;background:#eee}#c{position:absolute;left:40px;top:480px}</style>
<canvas id="cv" width="800" height="400"></canvas>
<div id="c">
<label><input type="checkbox"> Agree</label>
<label><input type="checkbox" onclick="return false"> Locked</label>
<button onmousedown="return false">Nothing</button>
<button onclick="document.title = 'hits:' + hits.join('|')">Report</button>
<label>Plain field <input type="text"></label>
<p id="gl"></p>
</div>
<script>
const hits = [];
document.getElementById('cv').addEventListener('click', (e) => {
  const r = e.target.getBoundingClientRect();
  hits.push(Math.round(e.clientX - r.left) + ',' + Math.round(e.clientY - r.top));
  e.target.getContext('2d').fillRect(e.clientX - r.left - 2, e.clientY - r.top - 2, 4, 4);
});
document.getElementById('gl').textContent = 'WebGL: ' + (document.createElement('canvas').getContext('webgl') ? 'yes' : 'no');
</script>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_clicks_exact_points_hears_what_changed_and_about_restarts() {
    if !have_browser() {
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().route("/", get(|| async { axum::response::Html(CLICKS) }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let click = |t: serde_json::Value| {
        let mut a = serde_json::json!({"action": "click"});
        a.as_object_mut()
            .unwrap()
            .extend(t.as_object().unwrap().clone());
        browser_call(a)
    };
    let h = harness(
        vec![
            browser_call(serde_json::json!({"action": "open", "url": url})),
            click(serde_json::json!({"ref": "e1"})),
            click(serde_json::json!({"ref": "e2"})),
            click(serde_json::json!({"ref": "e3"})),
            click(serde_json::json!({"x": 130, "y": 120})),
            click(serde_json::json!({"x": 740, "y": 375})),
            click(serde_json::json!({"ref": "e4"})),
            click(serde_json::json!({"x": 5000, "y": 10})),
            click(serde_json::json!({"ref": "e5"})),
            Reply::Text("clicked".into()),
            browser_call(serde_json::json!({"action": "snapshot"})),
            browser_call(serde_json::json!({"action": "snapshot"})),
            Reply::Text("looked again".into()),
        ],
        100_000,
    )
    .await;
    approver(&h, |_| ApprovalDecision::Allow);
    let s = h.session().await;
    h.send(&s.id, "click around").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let results = tool_results(&h.entries(&s.id).await);
    let get = |i: usize| results.get(i).cloned().unwrap_or_default();
    let (_, err, open) = get(0);
    assert!(!err, "{open}");
    for want in [
        "[e1] checkbox \"Agree\" [ ]",
        "[e2] checkbox \"Locked\" [ ]",
        "[e3] button \"Nothing\"",
        "[e4] button \"Report\"",
    ] {
        assert!(open.contains(want), "{want} missing from:\n{open}");
    }
    // Linux's private display still gives pages WebGL (in software).
    #[cfg(target_os = "linux")]
    assert!(open.contains("WebGL: yes"), "{open}");
    let (_, err, agreed) = get(1);
    assert!(
        !err && agreed.contains("it went from unchecked to checked"),
        "{agreed}"
    );
    let (_, err, locked) = get(2);
    assert!(!err && locked.contains("it is still unchecked"), "{locked}");
    let (_, err, nothing) = get(3);
    assert!(
        !err && nothing.contains("Nothing visibly changed"),
        "{nothing}"
    );
    // A point is clicked exactly where asked, not in the canvas's middle;
    // what the canvas draws can't be read from the page, and it says so.
    let (_, err, point) = get(4);
    assert!(
        !err && point.contains("at (130, 120)") && point.contains("take a `screenshot`"),
        "{point}"
    );
    let (_, err, report) = get(6);
    assert!(
        !err && report.contains("hits:90,70|700,325") && report.contains("the page changed"),
        "the canvas centre is 400,200: {report}"
    );
    let (_, err, outside) = get(7);
    assert!(
        err && outside.contains("outside the visible page"),
        "{outside}"
    );

    let (_, err, focused) = get(8);
    assert!(
        !err && focused.contains("focus moved to")
            && focused.contains("Plain field")
            && !focused.contains("Nothing visibly changed"),
        "{focused}"
    );

    // The browser dies between two steps: the agent's next step says so.
    let pid = h.engine.browser().pid().await.expect("running");
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    let gone = tokio::time::timeout(Duration::from_secs(10), async {
        while h.engine.browser().stats().running {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "the exit was noticed");
    h.send(&s.id, "look again").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let results = tool_results(&h.entries(&s.id).await);
    let (_, err, again) = results.get(9).cloned().unwrap_or_default();
    assert!(
        !err && again
            .contains("The browser stopped unexpectedly (it was killed (signal 9, SIGKILL))")
            && again.contains("anything typed into it and not yet submitted is gone")
            && again.contains("[e1] checkbox \"Agree\""),
        "{again}"
    );
    // Once.
    let (_, _, later) = results.get(10).cloned().unwrap_or_default();
    assert!(!later.contains("stopped unexpectedly"), "{later}");
    assert_eq!(h.engine.browser().stats().launches, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_browser_exits_keep_agent_tabs_and_gate_explicit_restarts() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness(
        vec![
            browser_call(serde_json::json!({"action": "open", "url": format!("{site}/")})),
            browser_call(serde_json::json!({"action": "open", "url": format!("{site}/two")})),
            Reply::Text("opened".into()),
        ],
        100_000,
    )
    .await;
    approver(&h, |_| ApprovalDecision::Allow);
    let s = h.session().await;
    h.send(&s.id, "open two pages").await;
    h.settle(&s.id).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
        .await
        .unwrap();
    let tabs = h.engine.browsers().agent_tabs(&s.id, &[]);
    assert_eq!(tabs.len(), 2);
    for count in 1..=4 {
        // Each tab gets a notice; don't consume the other tab's notice from
        // the previous crash before this kill has actually been detected.
        let mut events = h.engine.subscribe();
        let pid = h.engine.browser().pid().await.expect("running");
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();
        let notice = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(Event::Notice {
                    session_id,
                    message,
                    ..
                }) = events.recv().await
                    && session_id.as_deref() == Some(&s.id)
                    && message.contains("browser")
                {
                    break message;
                }
            }
        })
        .await
        .expect("agent-only tabs also tell the user");
        assert!(
            notice.contains("Anything typed and not submitted is gone"),
            "{notice}"
        );
        for tab in &tabs {
            h.mock.push(browser_call(
                serde_json::json!({"action": "snapshot", "tab": tab.key}),
            ));
        }
        h.mock.push(Reply::Text("looked".into()));
        h.send(&s.id, "look at the same tabs").await;
        h.settle(&s.id).await;
        tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(&s.id))
            .await
            .unwrap();
        let results = tool_results(&h.entries(&s.id).await);
        for (_, err, text) in results.iter().rev().take(2) {
            if count <= 3 {
                assert!(
                    !err && text.contains("stopped unexpectedly"),
                    "crash {count}: {text}"
                );
            } else {
                assert!(
                    *err && text.contains("keeps stopping") && text.contains("wait"),
                    "{text}"
                );
            }
        }
        let kept = h.engine.browsers().agent_tabs(&s.id, &[]);
        assert_eq!(kept.len(), 2);
        for t in &tabs {
            assert!(
                kept.iter().any(|k| k.key == t.key && k.url == t.url),
                "handles and addresses stay"
            );
        }
        assert_eq!(h.engine.browser().stats().launches, (count + 1).min(4));
        if count == 4 {
            assert!(notice.contains("Wait 60 seconds, then reload"), "{notice}");
            assert!(!h.engine.browser().stats().running);
        }
    }
}

#[tokio::test]
async fn private_session_closed_tab_tool_results_are_url_free_and_owner_checked() {
    let h = harness(Vec::new(), 100_000).await;
    approver(&h, |_| ApprovalDecision::Allow);
    let private: tessera_proto::SessionInfo = serde_json::from_value(
        h.engine
            .handle(
                Request::CreateSession {
                    cwd: Some(h.work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: true,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let owner_chat = h.session().await;
    let other = h.session().await;
    let address = "https://account.invalid/private/person?code=unknown-oauth-code#signed-fragment";
    h.engine
        .browsers()
        .new_agent_tab(&owner_chat.id, address, None)
        .unwrap();
    let old = h.engine.browsers().agent_tabs(&owner_chat.id, &[])[0]
        .key
        .clone();
    for _ in 0..5 {
        h.engine
            .browsers()
            .new_agent_tab(&owner_chat.id, "https://fixture.invalid/", None)
            .unwrap();
    }
    // Per-chat caps now refuse replacement. Exercise an explicit closure
    // to keep the owner-scoped, URL-free closure-result regression.
    assert!(h.engine.browsers().close_agent_tab(&owner_chat.id, &old));
    // Private chats intentionally cannot use the shared browser. Test that
    // ceiling as well as the closed-record owner check with equipped actors.
    for (chat, owner, denied) in [
        (&private.id, false, true),
        (&owner_chat.id, true, false),
        (&other.id, false, false),
    ] {
        h.mock.push(browser_call(
            serde_json::json!({"action": "snapshot", "tab": old}),
        ));
        h.mock.push(Reply::Text("finished".into()));
        h.send(chat, "look at the old tab").await;
        h.settle(chat).await;
        tokio::time::timeout(Duration::from_secs(30), h.engine.wait_idle(chat))
            .await
            .unwrap();
        let entries = h.entries(chat).await;
        let results = tool_results(&entries);
        let (_, err, message) = results.last().cloned().unwrap();
        assert!(err);
        assert!(
            !message.contains("https://")
                && !message.contains("account.invalid")
                && !message.contains("unknown-oauth-code")
                && !message.contains("signed-fragment"),
            "{message}"
        );
        assert_eq!(message.contains("was closed"), owner, "{message}");
        if denied {
            assert!(message.contains("Unknown tool `browser`"), "{message}");
        } else if !owner {
            assert!(message.starts_with("No tab `"), "{message}");
        }
    }
}
