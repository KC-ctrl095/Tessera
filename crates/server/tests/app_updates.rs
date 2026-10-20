//! An app update over a real (encrypted) connection: a paired device asks
//! whether there is a newer build, downloads it in pieces and gets exactly
//! the bytes the owner offered (the SHA-256 it was told).

use std::time::Duration;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use tessera_client::{Client, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::updates::Updates;
use tessera_proto::*;

#[tokio::test]
async fn a_device_downloads_the_offered_build_intact() {
    download_offered(false).await;
}

#[tokio::test]
async fn a_windows_device_downloads_nsis_over_the_pinned_encrypted_connection() {
    download_offered(true).await;
}

async fn download_offered(windows: bool) {
    let dir = tempfile::tempdir().unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });

    // A pretend phone app, a little over two download pieces.
    let platform = if windows {
        "windows-x86_64-desktop"
    } else {
        "android-arm64"
    };
    let mut apk = if windows {
        let mut head = vec![0; 2048];
        head[..2].copy_from_slice(b"MZ");
        head[60..64].copy_from_slice(&128u32.to_le_bytes());
        head[128..132].copy_from_slice(b"PE\0\0");
        head[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
        head[148..150].copy_from_slice(&224u16.to_le_bytes());
        head[150..152].copy_from_slice(&2u16.to_le_bytes());
        head[152..154].copy_from_slice(&0x10bu16.to_le_bytes());
        head[1024..1040].copy_from_slice(b"\xef\xbe\xad\xdeNullsoftInst");
        head
    } else {
        b"PK\x03\x04".to_vec()
    };
    apk.extend((0..(2 << 20) + 12_345).map(|i| (i % 251) as u8));
    apk.extend_from_slice(b"lib/arm64-v8a/libtessera_app_lib.so");
    let file = dir.path().join(if windows {
        "tessera-desktop-windows-x86_64-setup.exe"
    } else {
        "tessera-android-arm64.apk"
    });
    std::fs::write(&file, &apk).unwrap();
    let build = Updates::new(dir.path())
        .add(&file, None, Some("0.2.0"), Some("Updates in the app"))
        .unwrap();

    let (_, token) = engine.store().add_device("Pixel").unwrap();
    let key = tessera_server::load_identity(engine.paths()).unwrap();
    let phone = Client::connect(ServerConfig {
        endpoints: vec![format!("http://{addr}")],
        token,
        server_key: tessera_secure::b64(key.public()),
        ..Default::default()
    });
    phone.wait_connected(Duration::from_secs(10)).await.unwrap();
    assert!(phone.status().encrypted);

    let offered: Option<AppUpdate> = phone
        .call_as(Request::GetAppUpdate {
            platform: platform.into(),
            version: "0.1.0".into(),
            sha256: Some("00".repeat(32)),
            installed_at: Some(1),
        })
        .await
        .unwrap();
    let offered = offered.expect("a newer version is offered");
    assert_eq!(offered.sha256, build.sha256);
    assert_eq!(offered.notes.as_deref(), Some("Updates in the app"));

    let mut got = Vec::new();
    loop {
        let c: FileChunk = phone
            .call_as(Request::ReadAppUpdate {
                id: offered.id.clone(),
                offset: got.len() as u64,
                len: 1 << 20,
            })
            .await
            .unwrap();
        assert_eq!(c.size, offered.size);
        got.extend(
            base64::engine::general_purpose::STANDARD
                .decode(c.data)
                .unwrap(),
        );
        if c.eof {
            break;
        }
    }
    let hex: String = Sha256::digest(&got)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(hex, offered.sha256, "three pieces, intact");
    assert_eq!(got, apk);

    // Once it runs that build, there is nothing more.
    let now: Option<AppUpdate> = phone
        .call_as(Request::GetAppUpdate {
            platform: platform.into(),
            version: "0.2.0".into(),
            sha256: Some(hex),
            installed_at: None,
        })
        .await
        .unwrap();
    assert!(now.is_none());
    phone.close();
}
