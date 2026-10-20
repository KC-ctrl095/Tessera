use super::*;

pub(super) async fn engine() -> (Engine, tempfile::TempDir, Arc<Rt>) {
    let dir = tempfile::tempdir().unwrap();
    // This fixture tests explicit Auto actor authority. Choose Off deliberately
    // rather than depending on the test kernel's sandbox capabilities. Sandbox
    // and fallback tests opt back into Auto/Strict themselves.
    std::fs::write(dir.path().join("config.toml"),
        "default_model = \"fixture/model\"\n[security]\nsandbox = \"off\"\n[lsp]\nenabled = false\n[[provider]]\nid = \"fixture\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:1/v1\"\ndiscover = false\n[[provider.models]]\nid = \"model\"\n").unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    let rt = engine
        .0
        .create_session(
            Some(dir.path().display().to_string()),
            None,
            Some("Fixture".into()),
            SessionKind::Chat,
            None,
            Some(PermissionMode::Auto),
            None,
        )
        .unwrap();
    (engine, dir, rt)
}

/// A throwaway read-only tool context. No provider requests or user data.
pub(crate) async fn read_only_ctx() -> (ToolCtx, tempfile::TempDir) {
    let (engine, dir, rt) = engine().await;
    let session_id = rt.st.lock().info.id.clone();
    let ctx = ToolCtx {
        session_id,
        call_id: "fixture-call".into(),
        seq: 0,
        cwd: dir.path().to_path_buf(),
        cancel: CancellationToken::new(),
        host: Arc::new(Host(engine.0.clone(), true)),
        progress: Arc::new(|_| {}),
        depth: 1,
        cleared: None,
        admission: crate::tools::admission::ActionAdmission::denied(),
    };
    (ctx, dir)
}

/// A synthetic child that has acquired its slot, without running a model.
/// Creation leaves its budget absent while queued; action tests must model
/// the subsequent admission stage rather than bypass production checks.
pub(super) fn admit_helper(rt: &Rt) {
    let mut st = rt.st.lock();
    st.internal.subagent.as_mut().unwrap().queued = false;
    st.info.helper_queued = false;
    st.budget = Some(Budget {
        max_steps: 20,
        base: st.info.steps,
        used: 0,
        calls: Default::default(),
        soft_deadline: None,
        wrapped: false,
        closed: false,
    });
}

#[tokio::test]
async fn automatic_fallback_caps_existing_actors_but_explicit_off_does_not() {
    if crate::sandbox::support().complete() {
        eprintln!(
            "SKIP automatic_fallback_caps_existing_actors_but_explicit_off_does_not: full sandbox available"
        );
        return;
    }
    let (engine, _dir, rt) = engine().await;
    let ctx = ToolCtx {
        session_id: rt.st.lock().info.id.clone(),
        call_id: "fallback-fixture".into(),
        seq: 0,
        cwd: std::env::temp_dir(),
        cancel: CancellationToken::new(),
        host: Arc::new(Host(engine.0.clone(), false)),
        progress: Arc::new(|_| {}),
        depth: 0,
        cleared: None,
        admission: crate::tools::admission::ActionAdmission::denied(),
    };
    assert_eq!(ctx.host.permission(&ctx.session_id), PermissionMode::Auto);
    assert!(!ctx.host.unconfined_commands_ask());
    engine.0.config.write().security.sandbox = crate::config::SandboxMode::Auto;
    assert_eq!(ctx.host.permission(&ctx.session_id), PermissionMode::Ask);
    assert!(ctx.host.unconfined_commands_ask());
    let args = json!({"command":"printf fixture"});
    let risk = crate::tools::permission_risk(
        &crate::tools::bash::Bash,
        &args,
        &ctx,
        ctx.host.permission(&ctx.session_id),
    );
    assert!(matches!(risk, crate::tools::Risk::Ask { .. }), "{risk:?}");
    let out = crate::tools::Tool::run(&crate::tools::bash::Bash, args, &ctx).await;
    assert!(out.is_error && tessera_proto::plain_text(&out.content).contains("needs approval"));
    rt.st.lock().info.permission = PermissionMode::ReadOnly;
    assert_eq!(
        ctx.host.permission(&ctx.session_id),
        PermissionMode::ReadOnly
    );
    rt.st.lock().info.permission = PermissionMode::Auto;
    engine.0.config.write().security.sandbox = crate::config::SandboxMode::Off;
    assert_eq!(ctx.host.permission(&ctx.session_id), PermissionMode::Auto);
    assert!(!ctx.host.unconfined_commands_ask());
}
