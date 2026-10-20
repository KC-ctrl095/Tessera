//! Local TLS fixtures, never university sites or real accounts.
#![cfg(target_os = "linux")]
use super::*;
use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, BufReader};

struct Site {
    a: String,
    b: String,
    protected: String,
    config: String,
    _child: tokio::process::Child,
    _dir: tempfile::TempDir,
}

impl Site {
    async fn start() -> Self {
        Self::with_fault(None).await
    }

    async fn with_fault(fault: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/browser/normal-site.mjs"
            ))
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let raw = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let bin = std::env::var("CHROMIUM")
            .ok()
            .or_else(|| {
                [
                    format!(
                        "{}/.local/share/tessera-browser/chrome",
                        std::env::var("HOME").unwrap()
                    ),
                    "/usr/bin/chromium".into(),
                    "/usr/bin/google-chrome".into(),
                    "/usr/bin/chromium-browser".into(),
                ]
                .into_iter()
                .find(|p| std::path::Path::new(p).is_file())
            })
            .expect("the installed test browser has a known path");
        let wrapper = dir.path().join("fixture-chrome");
        // Trust only this run's ephemeral loopback certificate, not arbitrary
        // certificates. The product never gets a certificate-ignore switch.
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let script = if let Some(mode) = fault {
            format!(
                "#!/bin/sh\nexec node {} {} {} {} {} \"$@\"\n",
                quote(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/browser/fault-pipe.mjs"
                )),
                quote(mode),
                quote(&bin),
                quote(v["spki"].as_str().unwrap()),
                quote(dir.path().join("injected").to_str().unwrap())
            )
        } else {
            format!(
                "#!/bin/sh\nexec {} --ignore-certificate-errors-spki-list={} \"$@\"\n",
                quote(&bin),
                quote(v["spki"].as_str().unwrap())
            )
        };
        std::fs::write(&wrapper, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            a: v["a"].as_str().unwrap().into(),
            b: v["b"].as_str().unwrap().into(),
            protected: v["protectedUrl"].as_str().unwrap().into(),
            config: format!(
                "[server]\nbind = \"127.0.0.1:{}\"\n[browser]\npath = {:?}\nspeed = 12\n",
                v["protectedPort"].as_u64().unwrap(),
                wrapper.display().to_string()
            ),
            _child: child,
            _dir: dir,
        }
    }
}

async fn go(h: &Harness, v: &tessera_core::browser::Viewer, id: &str, url: String) {
    call(
        h,
        v,
        Request::BrowserNav {
            board_id: id.into(),
            action: BrowserNavAction::Go { url },
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_origin_oauth_and_first_inline_sign_in_guards() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/login", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("login paints");
    assert!(title_becomes(&h, &b.id, "Login").await);
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.popup
            && t.title == "Provider"
            && t.url.starts_with(&site.b))
        .await
    );
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("provider paints");
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| !t.popup
            && t.title == "Logged in across origins")
        .await,
        "postMessage reaches the other-origin opener"
    );
    go(&h, &v, &b.id, format!("{}/guard-popup", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Guard opener").await);
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("guard opener paints");
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.popup
            && t.title == "First inline guard ready")
        .await,
        "the popup's first inline get/create see the exact custom refusals"
    );
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Back,
        },
    )
    .await;
    assert!(title_becomes(&h, &b.id, "First inline guard ready").await);
    go(&h, &v, &b.id, format!("{}/blank-guard", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Blank guard opener").await);
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("blank opener paints");
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.popup
            && t.title == "First inline guard ready")
        .await,
        "synchronous document.write about:blank popup gets both exact first-inline refusals"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secure_cross_site_cookies_lax_redirects_and_remember_me_survive_restarts() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let mut h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/remember", site.b)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("saved page paints");
    assert!(title_becomes(&h, &b.id, "Saved").await);
    go(&h, &v, &b.id, format!("{}/remember-check", site.b)).await;
    assert!(title_becomes(&h, &b.id, "Cookie kept and storage kept").await);
    go(&h, &v, &b.id, format!("{}/embed", site.a)).await;
    assert!(
        title_becomes(&h, &b.id, "Cross-site cookie sent").await,
        "SameSite=None; Secure in a different-site iframe follows regular Chrome defaults"
    );
    go(&h, &v, &b.id, format!("{}/lax-set", site.b)).await;
    assert!(title_becomes(&h, &b.id, "Lax set").await);
    go(&h, &v, &b.id, format!("{}/lax-link", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Lax link").await);
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("redirect link paints");
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(title_becomes(&h, &b.id, "Lax redirect signed in").await);
    go(&h, &v, &b.id, format!("{}/remember-check", site.b)).await;
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: b.id.clone(),
        },
    )
    .await;
    h.engine.browser().shutdown().await;
    attach(&h, &v, &b.id, false).await;
    go(&h, &v, &b.id, format!("{}/remember-check", site.b)).await;
    assert!(
        title_becomes(&h, &b.id, "Cookie kept and storage kept").await,
        "browser restart keeps cookie/localStorage/IndexedDB"
    );
    h.engine.shutdown().await;
    h.engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    go(&h, &v, &b.id, format!("{}/remember-check", site.b)).await;
    assert!(
        title_becomes(&h, &b.id, "Cookie kept and storage kept").await,
        "server restart reuses its profile"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_identity_is_native_even_for_a_phone_viewport() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/identity", site.a)).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, true).await;
    assert!(state_becomes(&h, &b.id, |t| t.title.starts_with('{')).await);
    let state = h.engine.browser().find_state(&b.id).unwrap();
    let id: serde_json::Value = serde_json::from_str(&state.title).unwrap();
    assert_eq!(id["chrome"], true);
    assert_eq!(id["headless"], false);
    assert_eq!(id["android"], false);
    assert_eq!(id["header"], true);
    assert_eq!(id["webdriver"], true, "automation is not concealed");
    assert_eq!(id["guard"], true);
    assert_eq!(
        id["platform"],
        std::env::consts::OS
            .to_string()
            .replace("linux", "Linux x86_64")
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_selected_files_and_downloads_never_expose_host_paths() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/upload", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("file input paints");
    assert!(title_becomes(&h, &b.id, "Upload").await);
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.file_chooser.is_some()).await);
    let chooser = h
        .engine
        .browser()
        .find_state(&b.id)
        .unwrap()
        .file_chooser
        .unwrap();
    let file = tessera_proto::Attachment {
        name: "homework.txt".into(),
        mime: "text/plain".into(),
        data: base64::engine::general_purpose::STANDARD.encode(b"selected homework\n"),
    };
    assert!(
        h.engine
            .handle(
                Request::BrowserChooseFiles {
                    board_id: b.id.clone(),
                    chooser: chooser.clone(),
                    files: vec![file.clone()]
                },
                None
            )
            .await
            .is_err(),
        "an internal backend/tool is not a user's picker connection"
    );
    let result = h
        .engine
        .handle_as(
            Request::BrowserChooseFiles {
                board_id: b.id.clone(),
                chooser: chooser.clone(),
                files: vec![file],
            },
            None,
            Some(&v),
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(
        title_becomes(&h, &b.id, "Selected:homework.txt:true").await,
        "native trusted input/change events, not page-world JS"
    );
    // Stale capabilities are unusable, and traversal never names a host file.
    assert!(
        h.engine
            .handle_as(
                Request::BrowserChooseFiles {
                    board_id: b.id.clone(),
                    chooser,
                    files: vec![]
                },
                None,
                Some(&v)
            )
            .await
            .is_err()
    );
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.file_chooser.is_some()).await);
    let chooser = h
        .engine
        .browser()
        .find_state(&b.id)
        .unwrap()
        .file_chooser
        .unwrap();
    let unsafe_file = tessera_proto::Attachment {
        name: "../passwords".into(),
        mime: "text/plain".into(),
        data: "aG9tZXdvcms=".into(),
    };
    assert!(
        h.engine
            .handle_as(
                Request::BrowserChooseFiles {
                    board_id: b.id.clone(),
                    chooser: chooser.clone(),
                    files: vec![unsafe_file]
                },
                None,
                Some(&v)
            )
            .await
            .is_err()
    );
    go(&h, &v, &b.id, format!("{}/download-page", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Download").await);
    assert!(
        h.engine
            .handle_as(
                Request::BrowserChooseFiles {
                    board_id: b.id.clone(),
                    chooser,
                    files: vec![]
                },
                None,
                Some(&v)
            )
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    let file = loop {
        let value = h
            .engine
            .handle(
                Request::ListSharedFiles {
                    session_id: None,
                    board_id: Some(b.id.clone()),
                },
                None,
            )
            .await
            .unwrap();
        let found = value.as_array().unwrap().first().cloned();
        if let Some(f) = found {
            break f;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "download becomes an ordinary file card: {value}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(file["name"], "homework.txt");
    let chunk = h
        .engine
        .handle(
            Request::ReadSharedFile {
                id: file["id"].as_str().unwrap().into(),
                offset: 0,
                len: 1024,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(chunk["data"].as_str().unwrap())
            .unwrap(),
        b"Homework download\n"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn target_blank_keeps_normal_noopener_and_the_original_tab() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/tabs", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("tabs page paints");
    assert!(title_becomes(&h, &b.id, "Tabs").await);
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.tab_ids.len() == 2).await);
    let next = h.engine.browser().find_state(&b.id).unwrap().tab_ids[1].clone();
    attach(&h, &v, &next, false).await;
    assert!(
        state_becomes(&h, &next, |t| !t.popup
            && t.title == "Homework tab without opener")
        .await,
        "target=_blank becomes an independent tab without granting an opener handle"
    );
    assert!(
        title_becomes(&h, &b.id, "Tabs").await,
        "original page still exists without going Back"
    );
    call(&h, &v, Request::DeleteBoard { id: next }).await;
    assert!(
        state_becomes(&h, &b.id, |t| !t.popup && t.title == "Tabs").await,
        "original tab remains intact"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn noopener_form_keeps_post_body_origin_referrer_and_first_inline_guard() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/post-tab", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("form paints");
    assert!(title_becomes(&h, &b.id, "Post tab").await);
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.tab_ids.len() == 2).await);
    let next = h.engine.browser().find_state(&b.id).unwrap().tab_ids[1].clone();
    attach(&h, &v, &next, false).await;
    assert!(
        state_becomes(&h, &next, |t| !t.popup
            && t.title == "Post and referrer kept and guard ready")
        .await,
        "native noopener POST submission and exact first-inline key refusal survive independent tab bootstrap"
    );
    h.engine.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_storage_uses_chrome_defaults_and_persists_in_its_same_top_site() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/frame-store", site.a)).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    assert!(title_becomes(&h, &b.id, "Embedded storage saved").await);
    go(&h, &v, &b.id, format!("{}/frame-read", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Embedded storage kept").await);
    call(
        &h,
        &v,
        Request::BrowserDetach {
            board_id: b.id.clone(),
        },
    )
    .await;
    h.engine.browser().shutdown().await;
    attach(&h, &v, &b.id, false).await;
    go(&h, &v, &b.id, format!("{}/frame-read", site.a)).await;
    assert!(title_becomes(&h, &b.id, "Embedded storage kept").await);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_inline_popup_cannot_reach_the_harness_port() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/fetch-opener", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("network opener paints");
    assert!(title_becomes(&h, &b.id, "Fetch opener").await);
    tokio::time::sleep(Duration::from_millis(250)).await;
    click_at(&h, &v, &b.id, 60.0, 315.0).await;
    assert!(
        state_becomes(&h, &b.id, |t| t.popup && t.title == "Own port blocked").await,
        "first inline fetch has its interception guard before resume"
    );
    assert_eq!(
        reqwest::get(&site.protected)
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "0",
        "no request reached the throwaway protected listener"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_guard_or_fetch_never_runs_or_networks_the_popup() {
    if !have_browser() {
        return;
    }
    for (mode, noopener) in [
        ("guard-error", false),
        ("guard-id", false),
        ("network-hold-error", false),
        ("fetch-error", false),
        ("guard-error", true),
        ("guard-id", true),
        ("network-hold-error", true),
        ("fetch-error", true),
    ] {
        let site = Site::with_fault(Some(mode)).await;
        let h = harness_with(&site.config).await;
        let s = h.session().await;
        let path = if noopener {
            "fault-noopener"
        } else {
            "fault-opener"
        };
        let b = board(&h, &s.id, &format!("{}/{path}", site.a)).await;
        let (v, mut rx) = h.engine.browser_viewer();
        attach(&h, &v, &b.id, false).await;
        frame(&h, &mut rx, &v, &b.id)
            .await
            .expect("fault opener paints");
        assert!(title_becomes(&h, &b.id, "Fault opener").await);
        tokio::time::sleep(Duration::from_millis(250)).await;
        click_at(&h, &v, &b.id, 60.0, 315.0).await;
        let end = tokio::time::Instant::now() + Duration::from_secs(8);
        while !site._dir.path().join("injected").exists() {
            assert!(
                tokio::time::Instant::now() < end,
                "{mode}: fault not reached"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            state_becomes(&h, &b.id, |t| !t.popup && t.title == "Fault opener").await,
            "{mode}: failed popup closes without touching its opener"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            reqwest::get(&site.protected)
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "0",
            "{mode}: no network escaped initialization"
        );
        assert_eq!(
            h.engine.browser().find_state(&b.id).unwrap().title,
            "Fault opener",
            "{mode}: no first inline script ran"
        );
        h.engine.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_selection_context_find_are_isolated_and_password_safe() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/type", site.a)).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, true).await;
    assert!(title_becomes(&h, &b.id, "Type").await);
    let action = tessera_proto::BrowserPageAction::Find {
        text: "Long text".into(),
        backwards: false,
    };
    assert!(
        h.engine
            .handle(
                Request::BrowserPage {
                    board_id: b.id.clone(),
                    action: action.clone()
                },
                None
            )
            .await
            .is_err()
    );
    let result = h
        .engine
        .handle_as(
            Request::BrowserPage {
                board_id: b.id.clone(),
                action,
            },
            None,
            Some(&v),
        )
        .await
        .unwrap();
    assert_eq!(result["found"], true);
    let result = h
        .engine
        .handle_as(
            Request::BrowserPage {
                board_id: b.id.clone(),
                action: tessera_proto::BrowserPageAction::Selection,
            },
            None,
            Some(&v),
        )
        .await
        .unwrap();
    assert_eq!(result["text"], "Long text");
    go(&h, &v, &b.id, format!("{}/password", site.a)).await;
    assert!(title_becomes(&h, &b.id, "password-hidden").await);
    click_at(&h, &v, &b.id, 80.0, 32.0).await;
    for event in [
        BrowserInputEvent::Text {
            text: "private-test-password".into(),
        },
        BrowserInputEvent::Key {
            kind: tessera_proto::BrowserKeyKind::Down,
            key: "a".into(),
            code: "KeyA".into(),
            text: None,
            modifiers: 2,
        },
    ] {
        call(
            &h,
            &v,
            Request::BrowserInput {
                board_id: b.id.clone(),
                event,
            },
        )
        .await;
    }
    let result = h
        .engine
        .handle_as(
            Request::BrowserPage {
                board_id: b.id.clone(),
                action: tessera_proto::BrowserPageAction::Selection,
            },
            None,
            Some(&v),
        )
        .await
        .unwrap();
    assert_eq!(result["text"], "", "a password selection is never copied");
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_tab_groups_and_chunked_homework_survive_the_real_device_path() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let mut h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/type", site.a)).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    let next = call(
        &h,
        &v,
        Request::BrowserNewTab {
            board_id: b.id.clone(),
            url: format!("{}/file", site.a),
        },
    )
    .await;
    let next: tessera_proto::Whiteboard = serde_json::from_value(next).unwrap();
    assert_eq!(
        h.engine.browser().find_state(&b.id).unwrap().tab_ids,
        vec![b.id.clone(), next.id.clone()]
    );
    h.engine.shutdown().await;
    h.engine = tessera_core::Engine::open(
        tessera_core::config::Paths::at(h.dir.path()),
        tessera_core::EngineOptions { scheduler: false },
    )
    .await
    .unwrap();
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &next.id, false).await;
    assert!(title_becomes(&h, &next.id, "File").await);
    assert_eq!(
        h.engine.browser().find_state(&next.id).unwrap().tab_ids,
        vec![b.id.clone(), next.id.clone()]
    );
    click_at(&h, &v, &next.id, 50.0, 315.0).await;
    assert!(state_becomes(&h, &next.id, |t| t.file_chooser.is_some()).await);
    let chooser = h
        .engine
        .browser()
        .find_state(&next.id)
        .unwrap()
        .file_chooser
        .unwrap();
    let request = |action| Request::BrowserUpload {
        board_id: next.id.clone(),
        chooser: chooser.clone(),
        action,
    };
    use tessera_proto::{BrowserUploadAction as A, BrowserUploadFile};
    let started = call(
        &h,
        &v,
        request(A::Start {
            files: vec![BrowserUploadFile {
                name: "homework.bin".into(),
                size: 20 << 20,
            }],
        }),
    )
    .await;
    let token = started["upload"].as_str().unwrap().to_string();
    let (other, _) = h.engine.browser_viewer();
    assert!(
        h.engine
            .handle_as(
                request(A::Chunk {
                    upload: token.clone(),
                    index: 0,
                    data: "YQ==".into()
                }),
                None,
                Some(&other)
            )
            .await
            .is_err()
    );
    let data = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; 256 << 10]);
    for _ in 0..80 {
        call(
            &h,
            &v,
            request(A::Chunk {
                upload: token.clone(),
                index: 0,
                data: data.clone(),
            }),
        )
        .await;
    }
    call(&h, &v, request(A::Finish { upload: token })).await;
    assert!(title_becomes(&h, &next.id, "Uploaded 20971520").await);
    assert!(
        h.engine
            .browser()
            .find_state(&next.id)
            .unwrap()
            .file_chooser
            .is_none()
    );
    go(&h, &v, &next.id, format!("{}/type", site.a)).await;
    assert!(title_becomes(&h, &next.id, "Type").await);
    call(
        &h,
        &v,
        Request::BrowserPage {
            board_id: next.id.clone(),
            action: tessera_proto::BrowserPageAction::PrintPdf,
        },
    )
    .await;
    let files = call(
        &h,
        &v,
        Request::ListSharedFiles {
            session_id: None,
            board_id: Some(next.id.clone()),
        },
    )
    .await;
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["mime"] == "application/pdf")
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_link_context_opens_an_independent_noopener_tab() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/tabs", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("link paints");
    assert!(title_becomes(&h, &b.id, "Tabs").await);
    let found: tessera_proto::BrowserPageResult = serde_json::from_value(
        call(
            &h,
            &v,
            Request::BrowserPage {
                board_id: b.id.clone(),
                action: tessera_proto::BrowserPageAction::Context {
                    x: 60.0,
                    y: 315.0,
                    select_word: false,
                },
            },
        )
        .await,
    )
    .unwrap();
    let page: Whiteboard = serde_json::from_value(
        call(
            &h,
            &v,
            Request::BrowserNewTab {
                board_id: b.id.clone(),
                url: found.link.expect("plain link resolved in isolated world"),
            },
        )
        .await,
    )
    .unwrap();
    assert!(state_becomes(&h, &b.id, |t| t.tab_ids.len() == 2).await);
    let next = page.id;
    attach(&h, &v, &next, false).await;
    assert!(
        state_becomes(&h, &next, |t| !t.popup
            && t.title == "Homework tab without opener")
        .await
    );
    assert!(title_becomes(&h, &b.id, "Tabs").await);
    call(&h, &v, Request::DeleteBoard { id: next }).await;
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_pdf_stream_is_bounded_closed_and_never_offered() {
    if !have_browser() {
        return;
    }
    let site = Site::with_fault(Some("pdf-oversize")).await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/type", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("page ready");
    let error = h
        .engine
        .handle_as(
            Request::BrowserPage {
                board_id: b.id.clone(),
                action: tessera_proto::BrowserPageAction::PrintPdf,
            },
            None,
            Some(&v),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("larger than 16 MB"), "{error:?}");
    let marker = site._dir.path().join("injected");
    let end = Instant::now() + Duration::from_secs(3);
    while !marker.exists() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        format!("closed:{}", (16 << 20) + 1)
    );
    let files = call(
        &h,
        &v,
        Request::ListSharedFiles {
            session_id: None,
            board_id: Some(b.id.clone()),
        },
    )
    .await;
    assert_eq!(files.as_array().unwrap().len(), 0);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grouped_native_pages_switch_close_return_and_zoom_keep_painting() {
    if !have_browser() {
        return;
    }
    let (site, _) = site().await;
    let h = harness_with("[browser]\nquality = \"sharp\"\nspeed = 60").await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{site}/")).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("opener paints");
    click_at(&h, &v, &b.id, 300.0, 315.0).await;
    assert!(state_becomes(&h, &b.id, |t| t.tab_ids.len() == 2).await);
    let ids = h.engine.browser().find_state(&b.id).unwrap().tab_ids;
    let linked = ids.into_iter().find(|id| id != &b.id).unwrap();
    for (id, mobile, zoom, css) in [
        (&linked, true, 1.0, (412, 915)),
        (&b.id, false, 2.0, (512, 350)),
        (&linked, true, 1.25, (330, 732)),
        (&b.id, false, 1.0, (1024, 700)),
    ] {
        while rx.try_recv().is_ok() {}
        attach(&h, &v, id, mobile).await;
        call(
            &h,
            &v,
            Request::BrowserNav {
                board_id: id.clone(),
                action: BrowserNavAction::Zoom { factor: zoom },
            },
        )
        .await;
        let mut seen = Vec::new();
        for _ in 0..20 {
            let f = sized_frame(&h, &mut rx, &v, id)
                .await
                .expect("switched and zoomed page keeps painting");
            seen.push(f.clone());
            if f.2 == css {
                break;
            }
        }
        assert!(
            seen.iter().any(|f| f.2 == css),
            "{id} zoom {zoom} expected CSS {css:?}, seen {seen:?}"
        );
        call(
            &h,
            &v,
            Request::BrowserDetach {
                board_id: id.clone(),
            },
        )
        .await;
    }
    call(&h, &v, Request::DeleteBoard { id: linked }).await;
    attach(&h, &v, &b.id, false).await;
    call(
        &h,
        &v,
        Request::BrowserNav {
            board_id: b.id.clone(),
            action: BrowserNavAction::Zoom { factor: 2.0 },
        },
    )
    .await;
    let f = sized_frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("return after closing grouped page paints");
    assert_eq!(f.2, (512, 350));
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_descends_same_origin_frames_and_open_shadow_roots() {
    if !have_browser() {
        return;
    }
    let site = Site::start().await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/context-frames", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id)
        .await
        .expect("embedded page paints");
    assert!(title_becomes(&h, &b.id, "Context frames").await);
    for (x, y, word) in [(50.0, 52.0, "Frame"), (430.0, 52.0, "Shadow")] {
        let mut found = None;
        for _ in 0..50 {
            let r = call(
                &h,
                &v,
                Request::BrowserPage {
                    board_id: b.id.clone(),
                    action: tessera_proto::BrowserPageAction::Context {
                        x,
                        y,
                        select_word: true,
                    },
                },
            )
            .await;
            if r["link"].as_str() == Some(&format!("{}/type", site.a)) {
                found = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let found = found.expect("embedded link resolved with local coordinates");
        assert_eq!(found["text"], word, "{found:?}");
    }
    let error = h
        .engine
        .handle_as(
            Request::BrowserPage {
                board_id: b.id.clone(),
                action: tessera_proto::BrowserPageAction::Context {
                    x: 50.0,
                    y: 252.0,
                    select_word: true,
                },
            },
            None,
            Some(&v),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("cross-origin frame"), "{error:?}");
    h.engine.shutdown().await;
}

// Consume events while acknowledging paints, so readiness never depends on
// a sleep or on the producer escaping backpressure from discarded frames.
async fn fixture_state(
    h: &Harness,
    rx: &mut mpsc::Receiver<Event>,
    viewer: &tessera_core::browser::Viewer,
    board: &str,
    ready: impl Fn(&BrowserTab) -> bool,
) -> BrowserTab {
    // State publications use the engine broadcast, not the frame-only viewer
    // channel. Subscribe before checking current state so readiness cannot
    // fall between that check and the wait.
    let mut states = h.engine.subscribe();
    if let Some(tab) = h.engine.browser().find_state(board)
        && ready(&tab)
    {
        return tab;
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(Event::BrowserFrame { board_id, frame, .. }) if board_id == board => {
                        call(
                            h,
                            viewer,
                            Request::BrowserAck { board_id: board.into(), frame },
                        ).await;
                    }
                    Some(_) => {}
                    None => panic!("fixture viewer closed before readiness"),
                },
                event = states.recv() => match event {
                    Ok(Event::BrowserState { tab }) if tab.board_id == board && ready(&tab) => return tab,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(tab) = h.engine.browser().find_state(board) && ready(&tab) {
                            return tab;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("fixture states closed before readiness"),
                },
            }
        }
    })
    .await
    .expect("fixture readiness event within 15 seconds")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_uploads_roll_back_after_navigation_replacement_and_browser_error() {
    if !have_browser() {
        return;
    }
    use tessera_proto::{BrowserUploadAction as A, BrowserUploadFile};
    let site = Site::with_fault(Some("upload-failure")).await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/file?paint=navigation", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    let staging = h.dir.path().join("browser/tessera-uploads");
    for fault in ["navigation", "replacement", "browser"] {
        eprintln!("upload fixture {fault}: waiting for painted input");
        // This title is emitted after two animation frames, only once the
        // input has layout and is enabled. Then acknowledge an actual paint.
        fixture_state(&h, &mut rx, &v, &b.id, |t| {
            t.title == format!("File painted {fault}")
        })
        .await;
        frame(&h, &mut rx, &v, &b.id)
            .await
            .expect("ready input paints");
        click_at(&h, &v, &b.id, 40.0, 315.0).await;
        eprintln!("upload fixture {fault}: waiting for chooser");
        let chooser = fixture_state(&h, &mut rx, &v, &b.id, |t| t.file_chooser.is_some())
            .await
            .file_chooser
            .unwrap();
        let start = call(
            &h,
            &v,
            Request::BrowserUpload {
                board_id: b.id.clone(),
                chooser: chooser.clone(),
                action: A::Start {
                    files: vec![BrowserUploadFile {
                        name: "pending.txt".into(),
                        size: 1,
                    }],
                },
            },
        )
        .await;
        let token = start["upload"].as_str().unwrap().to_string();
        call(
            &h,
            &v,
            Request::BrowserUpload {
                board_id: b.id.clone(),
                chooser: chooser.clone(),
                action: A::Chunk {
                    upload: token.clone(),
                    index: 0,
                    data: "YQ==".into(),
                },
            },
        )
        .await;
        if fault == "navigation" {
            call(
                &h,
                &v,
                Request::BrowserNav {
                    board_id: b.id.clone(),
                    action: BrowserNavAction::Go {
                        url: format!("{}/type", site.a),
                    },
                },
            )
            .await;
            eprintln!("upload fixture {fault}: waiting for navigation title");
            fixture_state(&h, &mut rx, &v, &b.id, |t| t.title == "Type").await;
            call(
                &h,
                &v,
                Request::BrowserUpload {
                    board_id: b.id.clone(),
                    chooser: chooser.clone(),
                    action: A::Cancel {
                        upload: token.clone(),
                    },
                },
            )
            .await;
        } else {
            if fault == "replacement" {
                click_at(&h, &v, &b.id, 40.0, 315.0).await;
                eprintln!("upload fixture {fault}: waiting for replacement chooser");
                fixture_state(&h, &mut rx, &v, &b.id, |t| {
                    t.file_chooser.as_ref().is_some_and(|c| c != &chooser)
                })
                .await;
            }
            let error = h
                .engine
                .handle_as(
                    Request::BrowserUpload {
                        board_id: b.id.clone(),
                        chooser: chooser.clone(),
                        action: A::Finish {
                            upload: token.clone(),
                        },
                    },
                    None,
                    Some(&v),
                )
                .await
                .unwrap_err();
            assert!(
                error.message.contains(if fault == "replacement" {
                    "chooser closed"
                } else {
                    "fixture file input failure"
                }),
                "{fault}: {error:?}"
            );
        }
        assert!(
            std::fs::read_dir(&staging).unwrap().next().is_none(),
            "{fault}: pending staged files removed"
        );
        if let Some(next) = match fault {
            "navigation" => Some("replacement"),
            "replacement" => Some("browser"),
            _ => None,
        } {
            call(
                &h,
                &v,
                Request::BrowserNav {
                    board_id: b.id.clone(),
                    action: BrowserNavAction::Go {
                        url: format!("{}/file?paint={next}", site.a),
                    },
                },
            )
            .await;
        }
    }
    assert_eq!(
        std::fs::read_to_string(site._dir.path().join("injected")).unwrap(),
        "handoff-failed"
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_pdf_generation_closes_its_late_stream_without_reading() {
    if !have_browser() {
        return;
    }
    let site = Site::with_fault(Some("pdf-cancel")).await;
    let h = harness_with(&site.config).await;
    let s = h.session().await;
    let b = board(&h, &s.id, &format!("{}/type", site.a)).await;
    let (v, mut rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    frame(&h, &mut rx, &v, &b.id).await.expect("page ready");
    let request = h.engine.handle_as(
        Request::BrowserPage {
            board_id: b.id.clone(),
            action: tessera_proto::BrowserPageAction::PrintPdf,
        },
        None,
        Some(&v),
    );
    let marker = site._dir.path().join("injected");
    let started = async {
        let end = Instant::now() + Duration::from_secs(3);
        while !marker.exists() && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(marker.exists(), "print began");
    };
    tokio::select! {_ = request => panic!("print answered before delayed fixture"), _ = started => {}}
    // The request future was cancelled while Chrome was still generating.
    let end = Instant::now() + Duration::from_secs(3);
    while std::fs::read_to_string(&marker).ok().as_deref() != Some("closed:0")
        && Instant::now() < end
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "closed:0");
    h.engine.shutdown().await;
}
