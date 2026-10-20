//! Web view frames over real end-to-end encrypted connections: only the
//! connection that attached to a board gets its frames, and a connection
//! that goes away stops counting as a viewer. Skipped without Chromium.

use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n",
    )
    .unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

async fn device(engine: &tessera_core::Engine, url: &str, name: &str) -> Client {
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(&[url.to_string()], &BTreeMap::new(), &code, name)
        .await
        .unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![url.to_string()],
        headers: BTreeMap::new(),
        token: grant.token,
        server_key: grant.server_key,
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    client
}

/// A page that keeps changing, so frames keep coming.
async fn site() -> String {
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(|| async {
            axum::response::Html(
                "<!doctype html><title>Ticker</title><h1 id=t>0</h1><script>let n=0;setInterval(()=>{document.getElementById('t').textContent=++n},50)</script>",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_reach_only_the_attached_connection() {
    if !tessera_core::browser::installed(None) {
        eprintln!("no Chromium-family browser installed; skipping");
        return;
    }
    let (engine, url, _dir) = start().await;
    let phone = device(&engine, &url, "phone").await;
    let laptop = device(&engine, &url, "laptop").await;
    let session: SessionInfo = phone
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
    let board: Whiteboard = phone
        .call_as(Request::CreateBoard {
            session_id: session.id.clone(),
            title: "Ticker".into(),
            placed: false,
            content: BoardContent::Url { url: site().await },
        })
        .await
        .unwrap();
    let mut phone_events = phone.subscribe();
    let mut laptop_events = laptop.subscribe();
    let tab: BrowserTab = phone
        .call_as(Request::BrowserAttach {
            board_id: board.id.clone(),
            width: 400,
            height: 300,
            dpr: 2.0,
            mobile: true,
            pipelined: false,
            formats: vec![],
        })
        .await
        .unwrap();
    assert_eq!(tab.board_id, board.id);

    // The phone gets frames one at a time, each after its ack.
    let mut frames = Vec::new();
    let mut state_seen = false;
    let got = tokio::time::timeout(Duration::from_secs(20), async {
        while frames.len() < 5 {
            match phone_events.recv().await {
                Ok(ClientEvent::Event(Event::BrowserFrame {
                    board_id,
                    frame,
                    data,
                    width,
                    height,
                    mime,
                    ..
                })) if board_id == board.id => {
                    assert_eq!((width, height, mime.as_str()), (400, 300, "image/jpeg"));
                    assert!(data.len() > 500);
                    frames.push(frame);
                    phone
                        .call(Request::BrowserAck {
                            board_id: board_id.clone(),
                            frame,
                        })
                        .await
                        .unwrap();
                }
                Ok(ClientEvent::Event(Event::BrowserState { tab })) if tab.board_id == board.id => {
                    state_seen = true;
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(got.is_ok(), "frames: {frames:?}");
    assert!(frames.windows(2).all(|w| w[0] < w[1]), "{frames:?}");
    assert!(state_seen, "tab state is broadcast");

    // The laptop saw the state, but never a frame.
    let mut laptop_state = false;
    while let Ok(ev) = laptop_events.try_recv() {
        match ev {
            ClientEvent::Event(Event::BrowserFrame { .. }) => {
                panic!("a frame reached a connection that didn't attach")
            }
            ClientEvent::Event(Event::BrowserState { .. }) => laptop_state = true,
            _ => {}
        }
    }
    assert!(laptop_state);

    // Unacknowledged, the stream pauses (flow control).
    let paused = tokio::time::timeout(Duration::from_millis(1500), async {
        let mut n = 0;
        loop {
            if let Ok(ClientEvent::Event(Event::BrowserFrame { .. })) = phone_events.recv().await {
                n += 1;
                if n > 2 {
                    return n;
                }
            }
        }
    })
    .await;
    assert!(paused.is_err(), "frames kept coming without acks");

    // The phone goes away: nobody is watching any more.
    assert_eq!(engine.browser().stats().viewers, 1);
    drop(phone_events);
    drop(phone);
    let gone = tokio::time::timeout(Duration::from_secs(10), async {
        while engine.browser().stats().viewers != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "a closed connection detaches");
}

/// A page whose title mirrors what is typed into its (focused) field.
async fn echo_site() -> String {
    let app = axum::Router::new().route(
        "/",
        axum::routing::get(|| async {
            axum::response::Html(
                "<!doctype html><title>-</title><input id=f autofocus><script>const f=document.getElementById('f');f.focus();f.oninput=()=>{document.title=f.value||'-'}</script>",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_text_reaches_the_page_in_order() {
    if !tessera_core::browser::installed(None) {
        eprintln!("no Chromium-family browser installed; skipping");
        return;
    }
    let (engine, url, _dir) = start().await;
    let phone = device(&engine, &url, "phone").await;
    let session: SessionInfo = phone
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
    let board: Whiteboard = phone
        .call_as(Request::CreateBoard {
            session_id: session.id.clone(),
            title: "Echo".into(),
            placed: false,
            content: BoardContent::Url {
                url: echo_site().await,
            },
        })
        .await
        .unwrap();
    let mut events = phone.subscribe();
    let _: BrowserTab = phone
        .call_as(Request::BrowserAttach {
            board_id: board.id.clone(),
            width: 400,
            height: 300,
            dpr: 1.0,
            mobile: true,
            pipelined: false,
            formats: vec![],
        })
        .await
        .unwrap();
    // Wait for the page to load before typing.
    let loaded = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(ClientEvent::Event(Event::BrowserState { tab })) = events.recv().await
                && tab.board_id == board.id
                && tab.title == "-"
                && !tab.loading
            {
                return;
            }
        }
    })
    .await;
    assert!(loaded.is_ok(), "the page loaded");

    // Typed fast, the way a phone keyboard sends it: one request per
    // character, sent without waiting for the replies.
    let typed = "the quick brown fox jumps over the lazy dog";
    let sends = typed.chars().map(|c| {
        phone.call(Request::BrowserInput {
            board_id: board.id.clone(),
            event: BrowserInputEvent::Text {
                text: c.to_string(),
            },
        })
    });
    for r in futures::future::join_all(sends).await {
        r.unwrap();
    }
    let mut last = String::new();
    let done = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(ClientEvent::Event(Event::BrowserState { tab })) = events.recv().await
                && tab.board_id == board.id
            {
                last = tab.title.clone();
                if last.len() >= typed.len() {
                    return;
                }
            }
        }
    })
    .await;
    assert!(done.is_ok(), "the page got all the text (title: {last:?})");
    assert_eq!(last, typed, "characters arrived out of order");
}
