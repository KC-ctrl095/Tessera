//! Stop speech over an open encrypted connection, not a disconnected transport.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
use base64::Engine as _;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};
use tessera_client::{Client, ServerConfig};
use tessera_core::{
    Engine,
    config::Paths,
    voice::{catalog, wav},
};
use tessera_proto::{Request, ServerInfo};

async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn stop_voice_removes_engine_tree_and_scratch_with_connection_kept_open() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(engine.clone(), None).unwrap();
    let server =
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(
        std::slice::from_ref(&url),
        &BTreeMap::new(),
        &code,
        "voice fixture",
    )
    .await
    .unwrap();
    let config = ServerConfig {
        endpoints: vec![url],
        headers: BTreeMap::new(),
        token: grant.token,
        server_key: grant.server_key,
    };
    let client = Client::connect(config.clone());
    let other = Client::connect(config);
    client.wait_connected(Duration::from_secs(5)).await.unwrap();
    other.wait_connected(Duration::from_secs(5)).await.unwrap();
    let root = dir.path().join("voice");
    for program in [catalog::whisper().unwrap(), catalog::piper().unwrap()] {
        let folder = root.join(program.dir);
        std::fs::create_dir_all(&folder).unwrap();
        // Engine and descendant pid files are outside scratch so cleanup can be checked afterwards.
        let body = format!(
            "#!/bin/sh\necho $$ > {}/engine.pid\n/bin/sleep 60 & echo $! > {}/child.pid\nwait\n",
            root.display(),
            root.display()
        );
        std::fs::write(folder.join(program.bin), body).unwrap();
        std::fs::set_permissions(
            folder.join(program.bin),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("models")).unwrap();
    for model in ["tiny", "base"] {
        std::fs::write(
            root.join("models").join(catalog::model(model).unwrap().0),
            b"model",
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("voices")).unwrap();
    std::fs::write(root.join("voices/en_US-joe-medium.onnx"), b"voice").unwrap();
    std::fs::write(root.join("voices/en_US-joe-medium.onnx.json"), b"{}").unwrap();
    let audio =
        base64::engine::general_purpose::STANDARD.encode(wav::encode_pcm16(16_000, &[100; 16_000]));
    for (index, request) in [
        Request::Transcribe {
            audio: audio.clone(),
            mime: "audio/wav".into(),
            engine: Some("server".into()),
            language: None,
            partial: true,
            operation: Some("fixture".into()),
        },
        Request::Transcribe {
            audio,
            mime: "audio/wav".into(),
            engine: Some("server".into()),
            language: None,
            partial: false,
            operation: Some("fixture".into()),
        },
        Request::Speak {
            text: "test".into(),
            engine: Some("server".into()),
            operation: Some("fixture".into()),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let caller = client.clone();
        let work = tokio::spawn(async move { caller.call(request).await });
        wait_for(|| root.join("child.pid").exists()).await;
        if index == 0 {
            // Same id on a different live connection has no authority over this operation.
            other
                .call(Request::CancelVoice {
                    operation: "fixture".into(),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!work.is_finished());
        }
        let pids: Vec<u32> = ["engine.pid", "child.pid"]
            .into_iter()
            .map(|f| {
                std::fs::read_to_string(root.join(f))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap()
            })
            .collect();
        client
            .call(Request::CancelVoice {
                operation: "fixture".into(),
            })
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.message.contains("canceled"));
        wait_for(|| std::fs::read_dir(root.join("tmp")).unwrap().count() == 0).await;
        wait_for(|| {
            pids.iter().all(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
                stat.is_empty() || stat.split(") ").nth(1).is_some_and(|s| s.starts_with('Z'))
            })
        })
        .await;
        let _: ServerInfo = client
            .call_as(Request::Hello {
                client: "still connected".into(),
                version: "test".into(),
            })
            .await
            .unwrap();
        for f in ["engine.pid", "child.pid"] {
            std::fs::remove_file(root.join(f)).unwrap();
        }
    }
    client.close();
    other.close();
    server.abort();
}
