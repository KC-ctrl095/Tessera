//! Agent chooser uploads with real native input/change events and byte hashes.
use super::*;
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PAGE: &str = r#"<!doctype html><title>Upload fixtures</title>
<input type=file id=single aria-label="Single file" style="position:fixed;left:20px;top:60px;width:220px;height:40px">
<input type=file id=many multiple aria-label="Several files" style="position:fixed;left:20px;top:120px;width:220px;height:40px">
<input type=file id=hidden accept=".txt,text/plain" style="display:none">
<button onclick="document.getElementById('hidden').click()" style="position:fixed;left:20px;top:180px;width:220px;height:40px">Styled chooser</button>
<pre id=out>No files</pre><script>
for (const input of document.querySelectorAll('input')) input.onchange = async e => {
  const files = await Promise.all([...input.files].map(async f => ({name:f.name,size:f.size,hash:[...new Uint8Array(await crypto.subtle.digest('SHA-256',await f.arrayBuffer()))].map(b=>b.toString(16).padStart(2,'0')).join('')})));
  out.textContent=JSON.stringify({id:input.id,trusted:e.isTrusted,files});
};
</script>"#;

async fn upload_site(port: u16) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .unwrap();
    let app = Router::new().route("/", get(|| async { axum::response::Html(PAGE) }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}/"), task)
}

async fn call(h: &Harness, sid: &str, args: Value) -> (bool, String) {
    h.mock.push(browser_call(args));
    h.mock.push(Reply::Text("done".into()));
    h.send(sid, "Use the file picker").await;
    h.settle(sid).await;
    tokio::time::timeout(Duration::from_secs(60), h.engine.wait_idle(sid))
        .await
        .unwrap();
    let (_, err, text) = tool_results(&h.entries(sid).await).pop().unwrap();
    (err, text)
}

async fn mode(h: &Harness, sid: &str, permission: PermissionMode) {
    let mut request: Request =
        serde_json::from_value(json!({"method":"update_session","params":{"session_id":sid}}))
            .unwrap();
    if let Request::UpdateSession { permission: p, .. } = &mut request {
        *p = Some(permission);
    }
    h.engine.handle(request, None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_upload_single_multiple_hidden_accept_and_hashes() {
    if !have_browser() {
        return;
    }
    let (url, site) = upload_site(18730).await;
    let h = harness(Vec::new(), 100_000).await;
    let asked = approver(&h, |_| ApprovalDecision::Allow);
    let sid = h.session().await.id;
    let first = h._work_dir.path().join("notes.txt");
    let second = h._work_dir.path().join("more.bin");
    std::fs::write(&first, b"selected notes\n").unwrap();
    std::fs::write(&second, b"other notes\0\x01").unwrap();
    let open = call(&h, &sid, json!({"action":"open","url":url})).await;
    assert!(!open.0, "{open:?}");
    for (text, files, input) in [
        ("Single file", vec![first.clone()], "single"),
        ("Several files", vec![first.clone(), second.clone()], "many"),
        ("Styled chooser", vec![second.clone()], "hidden"),
    ] {
        if input == "many" {
            mode(&h, &sid, PermissionMode::Ask).await;
        }
        let clicked = call(&h, &sid, json!({"action":"click","text":text})).await;
        assert!(
            !clicked.0 && clicked.1.contains("file picker"),
            "{clicked:?}"
        );
        if input == "hidden" {
            assert!(clicked.1.contains(".txt,text/plain"), "{clicked:?}");
        }
        let out = call(&h, &sid, json!({"action":"upload","files":files})).await;
        assert!(
            !out.0 && out.1.contains("Sent ") && out.1.contains("http://127.0.0.1:18730"),
            "{out:?}"
        );
        assert!(
            out.1.contains(input) && out.1.contains("trusted\":true"),
            "{out:?}"
        );
        for path in files {
            let bytes = std::fs::read(&path).unwrap();
            let hash = format!("{:x}", Sha256::digest(&bytes));
            assert!(
                out.1.contains(&hash)
                    && out.1.contains(path.file_name().unwrap().to_str().unwrap()),
                "{out:?}"
            );
            assert!(
                !out.1
                    .contains(&base64::engine::general_purpose::STANDARD.encode(bytes)),
                "bytes are never tool output"
            );
        }
    }
    let questions = asked.lock().clone();
    let uploads = questions
        .iter()
        .filter(|q| q.reason.contains("Sends these files"))
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 3, "{questions:?}");
    assert!(uploads.iter().all(|q| q.once
        && (q.summary.contains(".txt") || q.summary.contains("more.bin"))
        && q.reason.contains("http://127.0.0.1:18730")));
    h.engine.shutdown().await;
    site.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_upload_denial_readonly_protected_secrets_and_symlinks() {
    if !have_browser() {
        return;
    }
    let (url, site) = upload_site(18731).await;
    let h = harness(Vec::new(), 100_000).await;
    let asked = approver(&h, |q| {
        if q.reason.contains("Sends these files") {
            ApprovalDecision::Deny
        } else {
            ApprovalDecision::Allow
        }
    });
    let sid = h.session().await.id;
    let first = h._work_dir.path().join("notes.txt");
    std::fs::write(&first, "ordinary fixture notes").unwrap();
    assert!(!call(&h, &sid, json!({"action":"open","url":url})).await.0);
    assert!(
        call(&h, &sid, json!({"action":"upload","files":[first]}))
            .await
            .1
            .contains("No file picker")
    );
    assert!(
        !call(&h, &sid, json!({"action":"click","x":40,"y":80}))
            .await
            .0
    );
    let out = call(&h, &sid, json!({"action":"upload","files":[first,first]})).await;
    assert!(out.0 && out.1.contains("one file"), "{out:?}");
    let protected = h.dir.path().join("fixture.txt");
    std::fs::write(&protected, "private fixture").unwrap();
    let key = h._work_dir.path().join("fixture.key");
    std::fs::write(&key, "fixture").unwrap();
    let env = h._work_dir.path().join(".env");
    std::fs::write(&env, "fixture").unwrap();
    let private = h._work_dir.path().join("saved.txt");
    std::fs::write(&private, "my saved fixture password value").unwrap();
    secret(&h, "FIXTURE_VALUE", "my saved fixture password value", &[]).await;
    let mut blocked = vec![protected.clone(), key, env, private];
    #[cfg(unix)]
    {
        let link = h._work_dir.path().join("linked.txt");
        std::os::unix::fs::symlink(&protected, &link).unwrap();
        blocked.push(link);
    }
    for path in blocked {
        let out = call(&h, &sid, json!({"action":"upload","files":[path]})).await;
        assert!(
            out.0 && (out.1.contains("protected") || out.1.contains("secret")),
            "{out:?}"
        );
    }
    assert_eq!(
        asked
            .lock()
            .iter()
            .filter(|q| q.reason.contains("Sends these files"))
            .count(),
        0
    );
    let denied = call(&h, &sid, json!({"action":"upload","files":[first]})).await;
    assert!(denied.0 && denied.1.contains("declined"), "{denied:?}");
    mode(&h, &sid, PermissionMode::ReadOnly).await;
    let ro = call(&h, &sid, json!({"action":"upload","files":[first]})).await;
    assert!(ro.0 && ro.1.contains("read-only"), "{ro:?}");
    mode(&h, &sid, PermissionMode::Auto).await;
    let out = call(&h, &sid, json!({"action":"snapshot"})).await;
    assert!(out.1.contains("No files"), "{out:?}");
    let auto = call(&h, &sid, json!({"action":"upload","files":[first]})).await;
    assert!(!auto.0 && auto.1.contains("Sent "), "{auto:?}");
    assert_eq!(
        asked
            .lock()
            .iter()
            .filter(|q| q.reason.contains("Sends these files"))
            .count(),
        1
    );
    h.engine.shutdown().await;
    site.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_upload_waiting_approval_binds_bytes_and_rejects_changed_page() {
    if !have_browser() {
        return;
    }
    let (url, site) = upload_site(18732).await;
    let h = harness(Vec::new(), 100_000).await;
    let sid = h.session().await.id;
    let file = h._work_dir.path().join("stable.txt");
    std::fs::write(&file, "approved snapshot").unwrap();
    let b = board(&h, &sid, &url).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    let engine = h.engine.clone();
    let mut events = engine.subscribe();
    let board_id = b.id.clone();
    let source = file.clone();
    let changed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = changed.clone();
    let answer = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let Event::ApprovalRequested {
                session_id,
                request,
            } = event
            {
                if request.reason.contains("Sends these files") {
                    if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        // The approved bytes are the already checked snapshot,
                        // not a new read made after this answer.
                        std::fs::write(&source, "later file contents").unwrap();
                    } else {
                        engine
                            .handle(
                                Request::BrowserNav {
                                    board_id: board_id.clone(),
                                    action: BrowserNavAction::Reload,
                                },
                                None,
                            )
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_millis(400)).await;
                    }
                }
                engine
                    .handle(
                        Request::RespondApproval {
                            session_id,
                            request_id: request.id,
                            decision: ApprovalDecision::AllowSession,
                        },
                        None,
                    )
                    .await
                    .unwrap();
            }
        }
    });
    assert!(
        !call(&h, &sid, json!({"action":"click","tab":b.id,"x":40,"y":80}))
            .await
            .0
    );
    let first = call(
        &h,
        &sid,
        json!({"action":"upload","tab":b.id,"files":[file]}),
    )
    .await;
    let hash = format!("{:x}", Sha256::digest(b"approved snapshot"));
    assert!(!first.0 && first.1.contains(&hash), "{first:?}");
    assert!(
        !call(&h, &sid, json!({"action":"click","tab":b.id,"x":40,"y":80}))
            .await
            .0
    );
    let next = call(
        &h,
        &sid,
        json!({"action":"upload","tab":b.id,"files":[file]}),
    )
    .await;
    assert!(
        next.0 && (next.1.contains("picker") || next.1.contains("page")),
        "{next:?}"
    );
    assert_eq!(
        changed.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "Allow session must not cover another upload"
    );
    answer.abort();
    h.engine.shutdown().await;
    site.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_chunked_picker_still_delivers_exact_bytes_and_cancel_clears() {
    if !have_browser() {
        return;
    }
    let (url, site) = upload_site(18733).await;
    let h = harness(Vec::new(), 100_000).await;
    let sid = h.session().await.id;
    let b = board(&h, &sid, &url).await;
    let (v, _rx) = h.engine.browser_viewer();
    attach(&h, &v, &b.id, false).await;
    click_at(&h, &v, &b.id, 40., 80.).await;
    assert!(state_becomes(&h, &b.id, |t| t.file_chooser.is_some()).await);
    let chooser = h
        .engine
        .browser()
        .find_state(&b.id)
        .unwrap()
        .file_chooser
        .unwrap();
    use tessera_proto::{BrowserUploadAction, BrowserUploadFile};
    let bytes = b"selected desktop and phone bytes";
    let value = super::call(
        &h,
        &v,
        Request::BrowserUpload {
            board_id: b.id.clone(),
            chooser: chooser.clone(),
            action: BrowserUploadAction::Start {
                files: vec![BrowserUploadFile {
                    name: "user-notes.txt".into(),
                    size: bytes.len() as u32,
                }],
            },
        },
    )
    .await;
    let upload = value["upload"].as_str().unwrap().to_string();
    super::call(
        &h,
        &v,
        Request::BrowserUpload {
            board_id: b.id.clone(),
            chooser: chooser.clone(),
            action: BrowserUploadAction::Chunk {
                upload: upload.clone(),
                index: 0,
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        },
    )
    .await;
    super::call(
        &h,
        &v,
        Request::BrowserUpload {
            board_id: b.id.clone(),
            chooser: chooser.clone(),
            action: BrowserUploadAction::Finish { upload },
        },
    )
    .await;
    let out = call(&h, &sid, json!({"action":"snapshot","tab":b.id})).await;
    assert!(
        !out.0
            && out.1.contains("user-notes.txt")
            && out.1.contains(&format!("{:x}", Sha256::digest(bytes))),
        "{out:?}"
    );
    click_at(&h, &v, &b.id, 40., 80.).await;
    assert!(state_becomes(&h, &b.id, |t| t.file_chooser.is_some()).await);
    let chooser = h
        .engine
        .browser()
        .find_state(&b.id)
        .unwrap()
        .file_chooser
        .unwrap();
    super::call(
        &h,
        &v,
        Request::BrowserChooseFiles {
            board_id: b.id.clone(),
            chooser,
            files: vec![],
        },
    )
    .await;
    assert!(state_becomes(&h, &b.id, |t| t.file_chooser.is_none()).await);
    h.engine.shutdown().await;
    site.abort();
}
