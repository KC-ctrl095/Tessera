//! Where web views run (no browser needed): in the browser of the machine
//! the user is using the chat on, when it has "Web views here" on and the
//! chat may use it; otherwise in this server's. A web view keeps its host;
//! a host that can't show it says so; "Open on the server" moves it there.

mod common;

use common::*;
use tessera_core::engine::NodeConn;
use tessera_proto::*;
use tokio::sync::mpsc;

/// A machine saying hello on a connection of its own (as `tessera node`).
async fn machine(
    h: &Harness,
    device: &str,
    name: &str,
    scope: MachineScope,
) -> (NodeConn, mpsc::Receiver<Event>) {
    let (conn, rx) = h.engine.node_conn(device);
    h.engine
        .node_request(
            Request::NodeHello {
                name: name.into(),
                os: "linux x86_64".into(),
                version: "0".into(),
                scope,
                generation: 1,
                sandbox: "on".into(),
                sandbox_ok: true,
                home: Some("/home/me".into()),
                computer: None,
                host: None,
                features: None,
                guard_key: None,
            },
            &conn,
        )
        .await
        .unwrap();
    (conn, rx)
}

fn web_scope() -> MachineScope {
    MachineScope {
        webviews: true,
        ..Default::default()
    }
}

async fn web_board(h: &Harness, session: &str, device: Option<&str>, url: &str) -> Whiteboard {
    let v = h
        .engine
        .handle(
            Request::CreateBoard {
                session_id: session.into(),
                title: "Web".into(),
                content: BoardContent::Url { url: url.into() },
                placed: true,
            },
            device,
        )
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn board(h: &Harness, id: &str) -> Whiteboard {
    serde_json::from_value(
        h.engine
            .handle(Request::GetBoard { id: id.into() }, None)
            .await
            .unwrap(),
    )
    .unwrap()
}

/// Open the chat on `device` (as an app does when it shows it).
async fn look(h: &Harness, session: &str, device: &str) {
    h.engine
        .handle(
            Request::GetSession {
                session_id: session.into(),
                since_seq: None,
            },
            Some(device),
        )
        .await
        .unwrap();
}

async fn attach(h: &Harness, board: &str, device: &str) -> Result<serde_json::Value, ApiError> {
    let (viewer, _rx) = h.engine.browser_viewer();
    h.engine
        .handle_as(
            Request::BrowserAttach {
                board_id: board.into(),
                width: 800,
                height: 600,
                dpr: 1.0,
                mobile: false,
                pipelined: true,
                formats: Vec::new(),
            },
            Some(device),
            Some(&viewer),
        )
        .await
}

/// The machine's next `node_browser` messages (what the server asked of it).
async fn asked(rx: &mut mpsc::Receiver<Event>) -> Option<Vec<NodeBrowserDown>> {
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
            Ok(Some(Event::NodeBrowser { msgs })) => return Some(msgs),
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
}

#[tokio::test]
async fn web_views_run_where_the_user_is() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (phone, _) = h.engine.store().add_device("Phone").unwrap();
    let (laptop, _) = h.engine.store().add_device("Laptop").unwrap();
    let (_desk_conn, mut desk_rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    // A machine without web views.
    let (_lap_conn, _lap_rx) = machine(&h, &laptop, "Laptop", MachineScope::default()).await;

    // Made on the desktop (a machine with web views): it runs there.
    let b1 = web_board(&h, &s.id, Some(&desktop), "https://example.com/").await;
    assert_eq!(b1.host.as_deref(), Some(desktop.as_str()));
    // Made on the phone: the server's.
    let b2 = web_board(&h, &s.id, Some(&phone), "https://example.org/").await;
    assert_eq!(b2.host, None);
    // On a machine without web views: the server's.
    let b3 = web_board(&h, &s.id, Some(&laptop), "https://example.net/").await;
    assert_eq!(b3.host, None);
    // Not a web page (and `local` apps are the server's own): no host.
    let v = h
        .engine
        .handle(
            Request::CreateBoard {
                session_id: s.id.clone(),
                title: "App".into(),
                content: BoardContent::Local {
                    port: 5173,
                    path: "/".into(),
                },
                placed: true,
            },
            Some(&desktop),
        )
        .await
        .unwrap();
    assert_eq!(serde_json::from_value::<Whiteboard>(v).unwrap().host, None);

    // The agent's: where the user is using the chat now.
    look(&h, &s.id, &desktop).await;
    assert_eq!(
        h.engine.browsers().place(&s.id, None).unwrap().as_deref(),
        Some(desktop.as_str())
    );
    look(&h, &s.id, &phone).await;
    assert_eq!(h.engine.browsers().place(&s.id, None).unwrap(), None);
    // Named, it goes there (if it may), or to the server.
    assert_eq!(
        h.engine
            .browsers()
            .place(&s.id, Some("desktop"))
            .unwrap()
            .as_deref(),
        Some(desktop.as_str())
    );
    assert_eq!(
        h.engine.browsers().place(&s.id, Some("server")).unwrap(),
        None
    );
    let e = h
        .engine
        .browsers()
        .place(&s.id, Some("Laptop"))
        .unwrap_err();
    assert!(e.contains("web views are off on Laptop"), "{e}");
    assert!(h.engine.browsers().place(&s.id, Some("Nope")).is_err());

    // A web view keeps its host, wherever the user goes on with the chat.
    look(&h, &s.id, &phone).await;
    assert_eq!(
        board(&h, &b1.id).await.host.as_deref(),
        Some(desktop.as_str())
    );

    // Showing it on the phone asks the desktop's browser to start (relayed).
    let phone_attach = tokio::spawn({
        let e = h.engine.clone();
        let id = b1.id.clone();
        let phone = phone.clone();
        async move {
            let (viewer, _rx) = e.browser_viewer();
            e.handle_as(
                Request::BrowserAttach {
                    board_id: id,
                    width: 400,
                    height: 800,
                    dpr: 2.0,
                    mobile: true,
                    pipelined: true,
                    formats: Vec::new(),
                },
                Some(&phone),
                Some(&viewer),
            )
            .await
        }
    });
    let msgs = asked(&mut desk_rx).await.expect("the desktop is asked");
    assert!(
        matches!(msgs.first(), Some(NodeBrowserDown::Start { .. })),
        "{msgs:?}"
    );
    assert!(
        !h.engine.browser().stats().running,
        "not the server's browser"
    );
    assert!(h.engine.browser().find_state(&b1.id).is_none());
    phone_attach.abort();
}

#[tokio::test]
async fn only_chats_that_may_use_the_machine_put_web_views_there() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    // Only the Sweeper bot may use it: a plain chat's web views stay on the server.
    let only = MachineScope {
        webviews: true,
        bots: Some(vec!["sweeper".into()]),
        ..Default::default()
    };
    let (_conn, _rx) = machine(&h, &desktop, "Desktop", only).await;
    let b = web_board(&h, &s.id, Some(&desktop), "https://example.com/").await;
    assert_eq!(b.host, None, "a plain chat may not use it");
    look(&h, &s.id, &desktop).await;
    assert_eq!(h.engine.browsers().place(&s.id, None).unwrap(), None);
    let e = h
        .engine
        .browsers()
        .place(&s.id, Some("Desktop"))
        .unwrap_err();
    assert!(e.contains("only certain bots"), "{e}");
    // A private chat never uses machines (made on the desktop: only it sees the chat).
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: None,
                private: true,
                lean: false,
            },
            Some(desktop.as_str()),
        )
        .await
        .unwrap();
    let private: SessionInfo = serde_json::from_value(v).unwrap();
    look(&h, &private.id, &desktop).await;
    assert_eq!(h.engine.browsers().place(&private.id, None).unwrap(), None);
}

#[tokio::test]
async fn an_offline_host_says_so_and_the_page_can_move_to_the_server() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (conn, rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let b = web_board(&h, &s.id, Some(&desktop), "https://example.com/").await;
    assert_eq!(b.host.as_deref(), Some(desktop.as_str()));
    // The desktop goes offline.
    drop(conn);
    drop(rx);
    let e = attach(&h, &b.id, &desktop).await.unwrap_err();
    assert!(e.message.contains("Desktop is offline"), "{}", e.message);
    // It keeps its host meanwhile; new web views from that device go to the server.
    assert_eq!(
        board(&h, &b.id).await.host.as_deref(),
        Some(desktop.as_str())
    );
    let b2 = web_board(&h, &s.id, Some(&desktop), "https://example.org/").await;
    assert_eq!(b2.host, None);
    // "Open on the server": it moves, and its tiles reload.
    let v = h
        .engine
        .handle(
            Request::BrowserToServer {
                board_id: b.id.clone(),
            },
            Some(&desktop),
        )
        .await
        .unwrap();
    let moved: Whiteboard = serde_json::from_value(v).unwrap();
    assert_eq!(moved.host, None);
    assert_eq!(moved.version, b.version + 1);
    assert_eq!(board(&h, &b.id).await.host, None);
    // Paused or web views switched off there: the same.
    let (conn2, _rx2) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let b3 = web_board(&h, &s.id, Some(&desktop), "https://example.net/").await;
    assert_eq!(b3.host.as_deref(), Some(desktop.as_str()));
    h.engine
        .handle(
            Request::SetMachinePaused {
                id: desktop.clone(),
                paused: true,
            },
            None,
        )
        .await
        .unwrap();
    let e = attach(&h, &b3.id, &desktop).await.unwrap_err();
    assert!(e.message.contains("Desktop is paused"), "{}", e.message);
    h.engine
        .handle(
            Request::SetMachinePaused {
                id: desktop.clone(),
                paused: false,
            },
            None,
        )
        .await
        .unwrap();
    h.engine
        .node_request(
            Request::NodeScope {
                scope: MachineScope::default(),
                name: None,
            },
            &conn2,
        )
        .await
        .unwrap();
    let e = attach(&h, &b3.id, &desktop).await.unwrap_err();
    assert!(
        e.message.contains("web views are off on Desktop"),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn only_a_machine_speaks_for_its_browser() {
    let h = harness(vec![], 100_000).await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (phone, _) = h.engine.store().add_device("Phone").unwrap();
    // Not from an ordinary request…
    let e = h
        .engine
        .handle(
            Request::NodeBrowser {
                seq: 1,
                msgs: vec![],
            },
            Some(&phone),
        )
        .await
        .unwrap_err();
    assert!(
        e.message.contains("only a machine's own connection"),
        "{}",
        e.message
    );
    // …nor from a connection that isn't a machine.
    let (conn, _rx) = h.engine.node_conn(&phone);
    assert!(
        h.engine
            .node_request(
                Request::NodeBrowser {
                    seq: 1,
                    msgs: vec![]
                },
                &conn
            )
            .await
            .is_err()
    );
    // A machine's own: fine (nothing to do with news for no browser).
    let (desk, _drx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    assert!(
        h.engine
            .node_request(
                Request::NodeBrowser {
                    seq: 1,
                    msgs: vec![]
                },
                &desk
            )
            .await
            .is_ok()
    );
    // And nothing a device sends can turn web views on there: the
    // server only keeps what the machine reports.
    let e = h
        .engine
        .handle(
            Request::NodeScope {
                scope: web_scope(),
                name: None,
            },
            Some(&phone),
        )
        .await
        .unwrap_err();
    assert!(
        e.message.contains("only a machine's own connection"),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn a_bot_needs_more_than_read_only_access_for_web_views_on_a_machine() {
    let h = harness(vec![], 100_000).await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (_conn, _rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let save = async |access: Option<MachineAccess>, id: &str| -> Bot {
        let bot = Bot {
            id: id.into(),
            name: "Scout".into(),
            handle: "scout".into(),
            avatar: String::new(),
            color: String::new(),
            role: String::new(),
            personality: String::new(),
            instructions: String::new(),
            model: None,
            thinking: None,
            permission: PermissionMode::Default,
            tools: ToolPreset::Research,
            allowed_tools: vec![],
            cwd: None,
            own_memory: true,
            share_user_profile: true,
            can_message: vec![],
            askable: None,
            knowledge: Vec::new(),
            starters: Vec::new(),
            machines: access.map(|a| BotMachines {
                default: MachineAccess::NoAccess,
                each: [(desktop.clone(), a)].into(),
            }),
            secrets: None,
            created_at: 0,
            updated_at: 0,
        };
        serde_json::from_value(
            h.engine
                .handle(Request::SaveBot { bot }, None)
                .await
                .unwrap(),
        )
        .unwrap()
    };
    // New, saved without the setting: no access.
    let scout = save(None, "").await;
    let v = h
        .engine
        .handle(
            Request::CreateSession {
                cwd: None,
                model: None,
                title: None,
                bot_id: Some(scout.id.clone()),
                private: false,
                lean: false,
            },
            None,
        )
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(v).unwrap();
    let e = h
        .engine
        .browsers()
        .place(&s.id, Some("Desktop"))
        .unwrap_err();
    assert!(e.contains("@scout has no access to it"), "{e}");
    // The machine the user is on isn't picked for it either.
    look(&h, &s.id, &desktop).await;
    assert_eq!(h.engine.browsers().place(&s.id, None).unwrap(), None);
    // Read only: a browser there is more than reading.
    save(Some(MachineAccess::ReadOnly), &scout.id).await;
    let e = h
        .engine
        .browsers()
        .place(&s.id, Some("Desktop"))
        .unwrap_err();
    assert!(
        e.contains("@scout may only read files") && e.contains("web views need more access"),
        "{e}"
    );
    // As the machine allows: there.
    save(Some(MachineAccess::Allowed), &scout.id).await;
    assert_eq!(
        h.engine
            .browsers()
            .place(&s.id, Some("Desktop"))
            .unwrap()
            .as_deref(),
        Some(desktop.as_str())
    );
}

/// "Show on <machine>'s screen": asked of the machine the web view runs on,
/// with the page's address; its answer comes back (a refusal in its words);
/// "Back to the tile" closes the window there. A web view on the server
/// can't: it says so plainly.
#[tokio::test]
async fn a_page_can_be_shown_on_its_machines_screen_and_back() {
    let h = harness(vec![], 100_000).await;
    let s = h.session().await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (phone, _) = h.engine.store().add_device("Phone").unwrap();
    let (desk, mut desk_rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let on_desk = web_board(&h, &s.id, Some(&desktop), "https://example.com/login").await;
    let on_server = web_board(&h, &s.id, Some(&phone), "https://example.org/").await;

    let show = |board: String, show: bool| {
        let e = h.engine.clone();
        let phone = phone.clone();
        tokio::spawn(async move {
            e.handle(
                Request::BrowserShowOnScreen {
                    board_id: board,
                    show,
                },
                Some(&phone),
            )
            .await
        })
    };
    // The machine's answers, numbered as its connection numbers them.
    let mut seq = 0;
    let mut reply = async |shown: bool, error: Option<&str>| {
        seq += 1;
        let msgs = vec![NodeBrowserUp::Screen {
            shown,
            error: error.map(str::to_string),
        }];
        h.engine
            .node_request(Request::NodeBrowser { seq, msgs }, &desk)
            .await
            .unwrap();
    };

    // On the server: no screen there, said plainly.
    let e = show(on_server.id.clone(), true).await.unwrap().unwrap_err();
    assert!(e.message.contains("no screen"), "{}", e.message);
    assert!(e.message.contains("your own browser"), "{}", e.message);

    // On the desktop: the machine is asked to show the page's address.
    let asking = show(on_desk.id.clone(), true);
    let msgs = asked(&mut desk_rx).await.expect("the desktop is asked");
    match msgs.as_slice() {
        [NodeBrowserDown::Screen { url: Some(u) }] => assert_eq!(u, "https://example.com/login"),
        other => panic!("{other:?}"),
    }
    reply(true, None).await;
    assert!(asking.await.unwrap().is_ok(), "shown");

    // A refusal there comes back in its words.
    let asking = show(on_desk.id.clone(), true);
    asked(&mut desk_rx).await.expect("asked again");
    reply(
        false,
        Some("nobody is signed in to this computer's desktop, so there's no screen to show the page on"),
    )
    .await;
    let e = asking.await.unwrap().unwrap_err();
    assert!(
        e.message.contains("Desktop:") && e.message.contains("nobody is signed in"),
        "{}",
        e.message
    );

    // Back to the tile: the window there is closed.
    let back = show(on_desk.id.clone(), false);
    let msgs = asked(&mut desk_rx).await.expect("asked to close it");
    assert!(
        matches!(msgs.as_slice(), [NodeBrowserDown::Screen { url: None }]),
        "{msgs:?}"
    );
    reply(false, None).await;
    assert!(back.await.unwrap().is_ok());
}

/// An agent's turn from before the user's Stop on a machine doesn't put web
/// views there any more (as for everything else there), also once that turn
/// has ended; the user's own web views were never held to it.
#[tokio::test]
async fn a_turn_from_before_a_stop_on_a_machine_doesnt_use_its_browser() {
    let h = harness(vec![], 100_000).await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (conn, _rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let s = h.session().await;
    h.mock.push(Reply::Tools(vec![(
        "bash".into(),
        serde_json::json!({"command": "sleep 3"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    h.send(&s.id, "wait").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(h.engine.browsers().place(&s.id, Some("Desktop")).is_ok());
    // Stopped and resumed there while the turn runs.
    h.engine
        .node_request(
            Request::NodeScope {
                scope: MachineScope {
                    stops: 1,
                    ..web_scope()
                },
                name: None,
            },
            &conn,
        )
        .await
        .unwrap();
    let e = h
        .engine
        .browsers()
        .place(&s.id, Some("Desktop"))
        .unwrap_err();
    assert!(
        e.contains("stopped on that computer after this turn began"),
        "{e}"
    );
    h.settle(&s.id).await;
    // Its turn over: still from before the Stop (something of it may go on).
    assert!(h.engine.browsers().place(&s.id, Some("Desktop")).is_err());
    // The user's own web view in that chat goes where they are, as before.
    look(&h, &s.id, &desktop).await;
    let b = web_board(&h, &s.id, Some(&desktop), "https://example.org/").await;
    assert_eq!(b.host.as_deref(), Some(desktop.as_str()));
}

/// A page approval doesn't authorize work from before this computer's Stop.
#[tokio::test]
async fn stopping_a_machine_while_browser_approval_waits_starts_no_cdp_run() {
    let h = harness(vec![], 100_000).await;
    let (desktop, _) = h.engine.store().add_device("Desktop").unwrap();
    let (conn, mut rx) = machine(&h, &desktop, "Desktop", web_scope()).await;
    let s = h.session().await;
    h.mock.push(Reply::Tools(vec![(
        "browser".into(),
        serde_json::json!({"action": "open", "on": "Desktop", "url": "http://127.0.0.1:18437/"}),
    )]));
    h.mock.push(Reply::Text("done".into()));
    let mut events = h.engine.subscribe();
    h.send(&s.id, "open the page").await;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Ok(Event::ApprovalRequested {
                session_id,
                request,
            }) = events.recv().await
            {
                h.engine
                    .node_request(
                        Request::NodeScope {
                            scope: MachineScope {
                                stops: 1,
                                ..web_scope()
                            },
                            name: None,
                        },
                        &conn,
                    )
                    .await
                    .unwrap();
                h.engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision: ApprovalDecision::Allow,
                        },
                        None,
                    )
                    .await
                    .unwrap();
                break;
            }
        }
    })
    .await
    .expect("browser approval wasn't requested");
    h.settle(&s.id).await;
    while let Ok(event) = rx.try_recv() {
        assert!(!matches!(event, Event::NodeBrowser { .. }), "{event:?}");
    }
    let results = tool_results(&h.entries(&s.id).await);
    assert!(
        results
            .iter()
            .any(|(_, error, text)| *error && text.contains("stopped")),
        "{results:?}"
    );
}
