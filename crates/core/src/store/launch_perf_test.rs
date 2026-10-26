// Included by store::tests. Opt-in store/serialization component measurement,
// complementary to the real-socket Chromium test (not a server RPC wall timer).
#[test]
#[ignore = "requires an explicitly marked synthetic launch store"]
fn launch_store_component_timings() {
    let home = std::path::PathBuf::from(
        std::env::var("TESSERA_LAUNCH_STORE").expect("TESSERA_LAUNCH_STORE"),
    );
    assert!(home.join("LAUNCH-PERF-THROWAWAY").is_file());
    let store = Store::open(&home.join("tessera.db")).unwrap();
    for n in 0..5 {
        let began = std::time::Instant::now();
        let full = store
            .list_sessions(false)
            .unwrap()
            .into_iter()
            .filter(|i| i.kind != SessionKind::Subagent)
            .collect::<Vec<_>>();
        let read_ms = began.elapsed().as_secs_f64() * 1000.0;
        let serialized = serde_json::to_vec(&full).unwrap();
        eprintln!("LAUNCH_STORE legacy round={n} rows={} bytes={} read_ms={read_ms:.3} read_serialize_ms={:.3}", full.len(), serialized.len(), began.elapsed().as_secs_f64()*1000.0);
        let began = std::time::Instant::now();
        let first = store.list_session_page(false, 100, None, &[]).unwrap();
        let read_ms = began.elapsed().as_secs_f64() * 1000.0;
        let serialized = serde_json::to_vec(&first).unwrap();
        eprintln!("LAUNCH_STORE first round={n} rows={} bytes={} read_ms={read_ms:.3} read_serialize_ms={:.3}", first.sessions.len(), serialized.len(), began.elapsed().as_secs_f64()*1000.0);
    }
}
