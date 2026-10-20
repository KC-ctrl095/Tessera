use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    _host: Arc<crate::browser::tests::FixtureHost>,
    _browsers: super::super::Browsers,
    tab: Arc<Tab>,
    viewer: super::super::Viewer,
    frames: mpsc::Receiver<Event>,
    link: Link,
    out: super::super::cdp::Outgoing,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (host, browsers) = crate::browser::tests::fixture_browsers(
            dir.path(),
            super::super::BrowserConfig::default(),
        );
        let tab = browsers
            .new_agent_tab("chat", "https://fixture.invalid/", None)
            .unwrap();
        let (viewer, frames) = browsers.viewer();
        let (cdp, out, _) = Cdp::channel();
        let link = Link {
            cdp,
            generation: 1,
            target: "page".into(),
            session: "page".into(),
        };
        tab.attach(
            viewer.id,
            Viewport::clamped(1024, 700, 1.0, false),
            Takes::default(),
        );
        tab.with_state(|st| {
            st.link = Some(link.clone());
            st.applied = Some((st.view, 1.0));
            st.streaming = Some((75, 1024, 700));
            st.runtime_on = true;
            // Already moving: rapid synthetic frames should not trigger the
            // unrelated adaptive restream while these tests count frame ACKs.
            st.motion.moving = true;
        });
        Self {
            _dir: dir,
            _host: host,
            _browsers: browsers,
            tab,
            viewer,
            frames,
            link,
            out,
        }
    }

    fn delivered(&mut self, width: u32, height: u32) -> u64 {
        self.tab.on_frame(&self.link, encoded(width, height));
        match self.frames.try_recv().unwrap() {
            Event::BrowserFrame {
                frame,
                width: w,
                height: h,
                ..
            } => {
                assert_eq!((w, h), (width, height));
                self.tab.ack(self.viewer.id, frame);
                frame
            }
            other => panic!("expected frame, got {other:?}"),
        }
    }

    fn rejected(&mut self, width: u32, height: u32) {
        self.tab.on_frame(&self.link, encoded(width, height));
        assert!(self.frames.try_recv().is_err());
    }

    fn ack_count(&mut self) -> usize {
        let mut count = 0;
        while let Ok(raw) = self.out.try_recv() {
            let command: Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(command["method"], "Page.screencastFrameAck");
            assert_eq!(command["params"]["sessionId"], 7);
            count += 1;
        }
        count
    }

    async fn command(&mut self, method: &str) -> Value {
        let raw = self.out.recv().await.unwrap();
        let command: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(command["method"], method);
        command
    }

    fn reply(&self, command: &Value) {
        self.link.cdp.feed(
            json!({"id":command["id"], "result":{}})
                .to_string()
                .as_bytes(),
        );
    }
}

fn encoded(width: u32, height: u32) -> Value {
    json!({"sessionId":7,"data":"encoded-fixture",
           "metadata":{"deviceWidth":width,"deviceHeight":height}})
}

#[tokio::test]
async fn old_size_frames_after_the_metrics_ack_are_dropped_and_acknowledged() {
    let mut f = Fixture::new();
    let first = f.delivered(1024, 700);
    assert_eq!(f.ack_count(), 1);
    f.tab.with_state(|st| st.zoom = 2.0);
    let tab = f.tab.clone();
    let link = f.link.clone();
    let applying = tokio::spawn(async move {
        tab.apply_view(&link).await.unwrap();
        tab.start_stream(&link).await;
    });
    let metrics = f.command("Emulation.setDeviceMetricsOverride").await;
    assert_eq!(metrics["params"]["width"], 512);
    assert_eq!(metrics["params"]["height"], 350);
    // Until ACK, the existing verified geometry can continue delivering.
    assert!(f.delivered(1024, 700) > first);
    assert_eq!(f.ack_count(), 1);
    assert!(!f.tab.st.lock().stream_stale);
    f.reply(&metrics);
    let stop = f.command("Page.stopScreencast").await;
    f.reply(&stop);
    let start = f.command("Page.startScreencast").await;
    f.reply(&start);
    applying.await.unwrap();
    assert!(!f.tab.st.lock().stream_stale);
    assert!(!f.tab.st.lock().motion.taking);
    assert!(f.tab.st.lock().still.is_none());
    f.rejected(1024, 700);
    assert!(f.delivered(512, 350) > first);
    assert_eq!(f.ack_count(), 2, "both old and delivered frames are ACKed");
}

#[tokio::test]
async fn known_geometry_accepts_one_dip_per_side_after_zoom_rounding() {
    let mut f = Fixture::new();
    f.tab.with_state(|st| {
        st.view = Viewport::clamped(501, 701, 1.0, false);
        st.zoom = 1.1;
        st.applied = Some((st.view, st.zoom));
    });
    for size in [(454, 636), (456, 638), (455, 637)] {
        f.delivered(size.0, size.1);
    }
    for size in [(1024, 700), (453, 637), (457, 637), (455, 635), (455, 639)] {
        f.rejected(size.0, size.1);
    }
    assert_eq!(
        f.ack_count(),
        8,
        "all accepted and rejected frames are ACKed"
    );
}

#[tokio::test]
async fn unknown_applied_geometry_accepts_frames_instead_of_fencing_delivery() {
    let mut f = Fixture::new();
    f.tab.with_state(|st| {
        st.zoom = 2.0;
        st.applied = None;
    });
    let mut last = 0;
    for size in [(1024, 700), (512, 350), (501, 701)] {
        let frame = f.delivered(size.0, size.1);
        assert!(frame > last);
        last = frame;
    }
    assert_eq!(f.ack_count(), 3);
}
