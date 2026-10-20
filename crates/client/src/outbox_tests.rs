use super::*;
use outbox::{MAX_BYTES, MAX_REQUEST_BYTES, MAX_REQUESTS};

fn fixture(count: usize, bytes: usize) -> Client {
    let (_, status) = watch::channel(ConnectionStatus::new(ConnState::Connected, None, None));
    let outbound = Arc::new(Outbox::with_limits(count, bytes));
    let stop = Arc::new(tokio_util_lite::Stop::default());
    Client {
        _lifetime: Arc::new(ClientLifetime {
            outbound: outbound.clone(),
            stop: stop.clone(),
        }),
        config: Arc::default(),
        outbound,
        waiters: Arc::default(),
        next_id: Arc::new(AtomicU64::new(1)),
        transport_epoch: Arc::new(AtomicU64::new(1)),
        instance_id: 1,
        events: broadcast::channel(16).0,
        status,
        stop,
        nudge: Arc::default(),
    }
}
fn mutation(n: usize) -> Request {
    Request::Send {
        session_id: "fixture".into(),
        content: vec![tessera_proto::ContentPart::text("x".repeat(n))],
        delivery: tessera_proto::Delivery::Auto,
        attachments: vec![],
    }
}

#[tokio::test]
async fn posts_report_count_and_bytes_overflow_without_dropping_mutations() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    for _ in 0..MAX_REQUESTS {
        client.post(Request::ListJobs).unwrap();
    }
    assert_eq!(client.post(mutation(1)).unwrap_err().code, ErrorCode::Busy);
    assert_eq!(client.outbound.used().0, MAX_REQUESTS);
    assert_eq!(client.outbound.used().2, MAX_REQUESTS);
    client.close();
    assert_eq!(client.outbound.used(), (0, 0, 0));
    assert!(client.post(mutation(1)).is_err());

    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    for _ in 0..3 {
        client.post(mutation(4 * 1024 * 1024)).unwrap();
    }
    assert_eq!(
        client.post(mutation(4 * 1024 * 1024)).unwrap_err().code,
        ErrorCode::Busy
    );
    let used = client.outbound.used();
    assert_eq!(used.0, 3);
    assert!(used.1 <= MAX_BYTES && used.1 > 12 * 1024 * 1024);
    assert_eq!(used.2, 3);
}

#[tokio::test]
async fn oversized_requests_are_explicit_errors_without_admission_or_clone() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    assert_eq!(
        client.post(mutation(MAX_REQUEST_BYTES)).unwrap_err().code,
        ErrorCode::BadRequest
    );
    assert_eq!(
        client
            .call(mutation(MAX_REQUEST_BYTES))
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );
    assert_eq!(client.outbound.used(), (0, 0, 0));
    assert!(client.waiters.lock().is_empty());
}

#[tokio::test(start_paused = true)]
async fn admission_backpressure_is_inside_the_rpc_timeout() {
    let client = fixture(1, MAX_BYTES);
    client.post(Request::ListJobs).unwrap();
    let start = tokio::time::Instant::now();
    assert_eq!(
        client
            .call_timeout(mutation(1), Duration::from_millis(100))
            .await
            .unwrap_err()
            .message,
        "request timed out"
    );
    assert_eq!(start.elapsed(), Duration::from_millis(100));
    assert_eq!(client.outbound.used().0, 1);
    assert!(client.waiters.lock().is_empty());
    drop(client.outbound.recv().await.unwrap());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_expiry_reclaim_queued_json_while_another_write_is_blocked() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    client.post(mutation(1024)).unwrap();
    let held_write = client.outbound.recv().await.unwrap();
    let initial = client.outbound.used();
    let c = client.clone();
    let aborted = tokio::spawn(async move {
        c.call_timeout(mutation(1024 * 1024), Duration::from_secs(5))
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(client.outbound.used().0, 2);
    assert_eq!(client.outbound.used().2, 1);
    aborted.abort();
    assert!(aborted.await.unwrap_err().is_cancelled());
    assert_eq!(
        client.outbound.used(),
        initial,
        "queue bytes released without writer polling"
    );
    let c = client.clone();
    let expired = tokio::spawn(async move {
        c.call_timeout(mutation(1024 * 1024), Duration::from_secs(1))
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        expired.await.unwrap().unwrap_err().message,
        "request timed out"
    );
    assert_eq!(client.outbound.used(), initial);
    drop(held_write);
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test(start_paused = true)]
async fn active_payload_and_rpc_hold_one_shared_lease_until_actual_release() {
    let client = fixture(1, MAX_BYTES);
    let c = client.clone();
    let call =
        tokio::spawn(async move { c.call_timeout(mutation(1024), Duration::from_secs(5)).await });
    let payload = client.outbound.recv().await.unwrap();
    assert_eq!(client.outbound.used().0, 1);
    assert_eq!(
        client.post(Request::ListJobs).unwrap_err().code,
        ErrorCode::Busy
    );
    client
        .waiters
        .lock()
        .remove(&payload.id)
        .unwrap()
        .send(Ok(serde_json::json!("ok")))
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), serde_json::json!("ok"));
    assert!(
        !payload.lease.cancelled.stopped(),
        "server reply is completion, not an aborted partial write"
    );
    assert_eq!(
        client.outbound.used().0,
        1,
        "frame still retains its counted lease"
    );
    drop(payload);
    assert_eq!(client.outbound.used(), (0, 0, 0));
    client.post(Request::ListJobs).unwrap();
}

#[tokio::test(start_paused = true)]
async fn byte_budget_waiters_and_shutdown_release_partial_admission() {
    let client = fixture(2, 1024);
    client.post(mutation(600)).unwrap();
    let c = client.clone();
    let call =
        tokio::spawn(async move { c.call_timeout(mutation(600), Duration::from_secs(1)).await });
    tokio::task::yield_now().await;
    assert_eq!(
        client.outbound.used().0,
        1,
        "byte waiters do not reserve a shared count slot"
    );
    client.close();
    assert!(call.await.unwrap().is_err());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test(start_paused = true)]
async fn waiting_admission_progresses_when_a_prior_payload_is_released() {
    let client = fixture(1, MAX_BYTES);
    client.post(Request::ListJobs).unwrap();
    let c = client.clone();
    let call =
        tokio::spawn(async move { c.call_timeout(mutation(1024), Duration::from_secs(5)).await });
    tokio::task::yield_now().await;
    assert!(
        client.waiters.lock().is_empty(),
        "backpressured request is not in the manager's queue yet"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    drop(client.outbound.recv().await.unwrap());
    let payload = client.outbound.recv().await.unwrap();
    assert!(serde_json::from_str::<ClientMessage>(&payload.text).is_ok());
    client
        .waiters
        .lock()
        .remove(&payload.id)
        .unwrap()
        .send(Ok(serde_json::json!("admitted")))
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), serde_json::json!("admitted"));
    drop(payload);
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

fn upload(raw_sizes: &[usize]) -> Request {
    Request::Send {
        session_id: "fixture".into(),
        content: vec![],
        delivery: tessera_proto::Delivery::Auto,
        attachments: raw_sizes
            .iter()
            .enumerate()
            .map(|(i, n)| tessera_proto::Attachment {
                name: format!("image-{i}.png"),
                mime: "image/png".into(),
                data: tessera_secure::b64(&vec![0; *n]),
            })
            .collect(),
    }
}

#[tokio::test]
async fn seven_mib_file_and_full_server_upload_limit_are_admitted() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    client.post(upload(&[7 * 1024 * 1024])).unwrap();
    let file = client.outbound.recv().await.unwrap();
    assert!(file.text.len() > 8 * 1024 * 1024 && file.text.len() < MAX_BYTES);
    assert!(!client.outbound.large_in_use());
    drop(file);
    client
        .post(upload(&[tessera_proto::MAX_FILE_ATTACHMENT_BYTES; 2]))
        .unwrap();
    let maximum = client.outbound.recv().await.unwrap();
    assert!(maximum.text.len() > MAX_BYTES && maximum.text.len() <= MAX_REQUEST_BYTES);
    assert!(client.outbound.large_in_use());
    drop(maximum);
    assert!(!client.outbound.large_in_use());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test(start_paused = true)]
async fn multi_image_upload_holds_exclusive_slot_until_the_active_payload_is_released() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    client.post(upload(&[6 * 1024 * 1024; 3])).unwrap(); // 18 MiB raw, 24 MiB serialized
    let first = client.outbound.recv().await.unwrap();
    assert!(client.outbound.large_in_use());
    assert_eq!(
        client.post(upload(&[6 * 1024 * 1024; 3])).unwrap_err().code,
        ErrorCode::Busy
    );
    client.post(Request::ListJobs).unwrap(); // ordinary work retains its independent budget
    drop(client.outbound.recv().await.unwrap());
    let c = client.clone();
    let second = tokio::spawn(async move {
        c.call_timeout(upload(&[6 * 1024 * 1024; 3]), Duration::from_secs(5))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        client.waiters.lock().is_empty(),
        "second large request waits, not queued alongside the first"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(!second.is_finished());
    drop(first);
    let payload = client.outbound.recv().await.unwrap();
    client
        .waiters
        .lock()
        .remove(&payload.id)
        .unwrap()
        .send(Ok(serde_json::json!("uploaded")))
        .unwrap();
    assert_eq!(
        second.await.unwrap().unwrap(),
        serde_json::json!("uploaded")
    );
    assert!(
        client.outbound.large_in_use(),
        "reply alone cannot release active frame's lease"
    );
    drop(payload);
    assert!(!client.outbound.large_in_use());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test(start_paused = true)]
async fn second_large_upload_can_expire_inside_its_rpc_deadline() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    client.post(upload(&[6 * 1024 * 1024; 3])).unwrap();
    let start = tokio::time::Instant::now();
    assert_eq!(
        client
            .call_timeout(upload(&[6 * 1024 * 1024; 3]), Duration::from_secs(1))
            .await
            .unwrap_err()
            .message,
        "request timed out"
    );
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    assert_eq!(client.outbound.used().0, 1);
    assert!(client.waiters.lock().is_empty());
    client.close();
    assert!(!client.outbound.large_in_use());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}

#[tokio::test]
async fn raw_upload_over_the_server_max_is_rejected_before_queueing() {
    let client = fixture(MAX_REQUESTS, MAX_BYTES);
    let error = client.post(upload(&[17 * 1024 * 1024; 3])).unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("50 MiB per message"));
    assert_eq!(client.outbound.used(), (0, 0, 0));
    assert!(!client.outbound.large_in_use());
}

#[tokio::test(start_paused = true)]
async fn saturated_large_waiters_leave_ordinary_count_admission_available() {
    let client = fixture(MAX_REQUESTS, 1024);
    client.post(mutation(2048)).unwrap();
    let first = client.outbound.recv().await.unwrap();
    let mut waiters = Vec::new();
    for _ in 0..MAX_REQUESTS {
        let c = client.clone();
        waiters.push(tokio::spawn(async move {
            c.call_timeout(mutation(2048), Duration::from_secs(30))
                .await
        }));
    }
    tokio::task::yield_now().await;
    assert_eq!(
        client.outbound.used().0,
        1,
        "all large waiters stop before shared count admission"
    );
    client.post(Request::ListJobs).unwrap();
    assert_eq!(client.outbound.used().0, 2);
    for waiter in waiters {
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
    }
    assert_eq!(client.outbound.used().0, 2);
    drop(first);
    client.close();
    assert!(!client.outbound.large_in_use());
    assert_eq!(client.outbound.used(), (0, 0, 0));
}
