//! Final-scope regression fixtures. Throwaway profiles and channel CDP only.
use super::*;
use tessera_proto::{NodeBrowserDown as Down, NodeBrowserUp as Up};

fn feed(remote: &remote::RemoteHost, run: u64, value: Value) {
    remote.feed(vec![Up::Cdp {
        run,
        messages: vec![value.to_string()],
    }]);
}

#[tokio::test(start_paused = true)]
async fn benchmark_attaches_during_startup_saves_numeric_speed_and_retries_a_failed_measurement() {
    let tmp = tempfile::tempdir().unwrap();
    let (_host, browsers) = tests::fixture_browsers(tmp.path(), BrowserConfig::default());
    let server = browsers.server();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let b = Browser::remote(
        server.0.host.clone(),
        server.0.events.clone(),
        tmp.path().join("remote"),
        server.0.hub.clone(),
        Arc::new(tests::RemoteFixture(tx)),
    );
    let epoch = Instant::now();
    b.0.measurement_times
        .lock()
        .extend((0..18).map(|i| epoch + Duration::from_micros(i * 16_384)));
    let remote = b.0.remote.clone().unwrap();
    let creates = Arc::new(AtomicU64::new(0));
    let count = creates.clone();
    let driver = tokio::spawn(async move {
        let mut frame = 0;
        while let Some(batch) = rx.recv().await {
            for command in batch {
                match command {
                    Down::Start { run, .. } => remote.feed(vec![Up::Started {
                        run,
                        product: "Chrome/155".into(),
                        speed_key: "fixture".into(),
                        max_tabs: 8,
                    }]),
                    Down::Stop { run } => remote.feed(vec![Up::Stopped {
                        run,
                        reason: None,
                        crashed: false,
                    }]),
                    Down::Cdp { run, messages } => {
                        for raw in messages {
                            let m: Value = serde_json::from_str(&raw).unwrap();
                            let method = m["method"].as_str().unwrap();
                            let result = match method {
                                "Emulation.getScreenInfos" => {
                                    json!({"id":m["id"],"error":{"code":-32000,"message":"optional unavailable"}})
                                }
                                "Target.createTarget" => {
                                    if count.fetch_add(1, Ordering::Relaxed) == 0 {
                                        json!({"id":m["id"],"error":{"code":-32000,"message":"first benchmark unavailable"}})
                                    } else {
                                        feed(
                                            &remote,
                                            run,
                                            json!({"method":"Target.attachedToTarget","params":{"sessionId":"benchmark","waitingForDebugger":true,"targetInfo":{"type":"page","targetId":"probe"}}}),
                                        );
                                        json!({"id":m["id"],"result":{"targetId":"probe"}})
                                    }
                                }
                                _ => json!({"id":m["id"],"result":{"product":"Chrome/155"}}),
                            };
                            feed(&remote, run, result);
                            match method {
                                "Page.navigate" => feed(
                                    &remote,
                                    run,
                                    json!({"sessionId":"benchmark","method":"Page.loadEventFired","params":{}}),
                                ),
                                "Page.startScreencast" => {
                                    frame = 1;
                                    feed(
                                        &remote,
                                        run,
                                        json!({"sessionId":"benchmark","method":"Page.screencastFrame","params":{"sessionId":frame}}),
                                    );
                                }
                                "Page.screencastFrameAck" if frame < 18 => {
                                    frame += 1;
                                    feed(
                                        &remote,
                                        run,
                                        json!({"sessionId":"benchmark","method":"Page.screencastFrame","params":{"sessionId":frame}}),
                                    );
                                }
                                "Browser.close" => remote.feed(vec![Up::Stopped {
                                    run,
                                    reason: None,
                                    crashed: false,
                                }]),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    });
    b.start().await.unwrap();
    let path = render::speed_file(&b.0.profile);
    assert!(render::load(&path).unwrap().ms_per_mpx.is_none());
    b.stop().await;
    b.start().await.unwrap();
    let saved = render::load(&path).unwrap();
    assert_eq!(
        saved.ms_per_mpx,
        Some(4.0),
        "known 16.384 ms frame gaps over 4.096 megapixels"
    );
    assert!(b.0.measurement_times.lock().is_empty());
    assert_eq!(
        creates.load(Ordering::Relaxed),
        2,
        "cached None must retry measurement"
    );
    assert!(
        b.0.attached.lock().is_empty(),
        "the startup event was consumed exactly once"
    );
    b.stop().await;
    b.start().await.unwrap();
    assert_eq!(
        creates.load(Ordering::Relaxed),
        2,
        "numeric cache avoids another measurement"
    );
    b.stop().await;
    driver.abort();
}

#[tokio::test(start_paused = true)]
async fn cached_remote_speed_recomputes_the_fallback_even_with_screens_disabled() {
    for virtual_screens in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = BrowserConfig {
            virtual_screens,
            ..Default::default()
        };
        let expected = render::global(cfg.quality, cfg.scale, Some(40.0));
        assert_ne!(expected, render::global(cfg.quality, cfg.scale, None));
        let (_host, browsers) = tests::fixture_browsers(tmp.path(), cfg);
        let server = browsers.server();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Browser::remote(
            server.0.host.clone(),
            server.0.events.clone(),
            tmp.path().join("remote"),
            server.0.hub.clone(),
            Arc::new(tests::RemoteFixture(tx)),
        );
        private_dir(&b.0.profile).unwrap();
        render::save(
            &render::speed_file(&b.0.profile),
            &render::Measured {
                key: "machine|Chrome/155|fixture".into(),
                ms_per_mpx: Some(40.0),
                no_screens: true,
            },
        );
        let remote = b.0.remote.clone().unwrap();
        let scales = Arc::new(Mutex::new(Vec::new()));
        let seen = scales.clone();
        let driver = tokio::spawn(async move {
            while let Some(batch) = rx.recv().await {
                for command in batch {
                    match command {
                        Down::Start { run, scale, .. } => {
                            seen.lock().push(scale);
                            remote.feed(vec![Up::Started {
                                run,
                                product: "Chrome/155".into(),
                                speed_key: "fixture".into(),
                                max_tabs: 8,
                            }]);
                        }
                        Down::Stop { run } => remote.feed(vec![Up::Stopped {
                            run,
                            reason: None,
                            crashed: false,
                        }]),
                        Down::Cdp { run, messages } => {
                            for raw in messages {
                                let m: Value = serde_json::from_str(&raw).unwrap();
                                let method = m["method"].as_str().unwrap();
                                assert_ne!(
                                    method, "Emulation.getScreenInfos",
                                    "cached refusal is honored"
                                );
                                assert_ne!(
                                    method, "Target.createTarget",
                                    "cached numeric speed is not measured again"
                                );
                                feed(
                                    &remote,
                                    run,
                                    json!({"id":m["id"],"result":{"product":"Chrome/155"}}),
                                );
                                if method == "Browser.close" {
                                    remote.feed(vec![Up::Stopped {
                                        run,
                                        reason: None,
                                        crashed: false,
                                    }]);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        b.start().await.unwrap();
        assert_eq!(scales.lock().last().copied().flatten(), Some(expected));
        assert_eq!(b.global_scale(), expected);
        assert_eq!(*b.0.speed.lock(), Some(40.0));
        assert_eq!(b.0.recovery.lock().failures, 0);
        b.stop().await;
        driver.abort();
    }
}
