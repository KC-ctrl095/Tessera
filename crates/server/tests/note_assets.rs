//! Pictures in notes over real connections: a paired device's picture comes
//! to another paired device over the encrypted connection, and to nobody
//! else (no address serves it, a removed device can't connect to ask).

use std::time::Duration;

use base64::Engine as _;
use tessera_client::{Client, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n",
    )
    .unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

/// A paired device's encrypted connection: its id, token and client.
async fn device(engine: &tessera_core::Engine, url: &str, name: &str) -> (String, String, Client) {
    let (id, token) = engine.store().add_device(name).unwrap();
    let client = connect(engine, url, &token);
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    assert!(client.status().encrypted);
    (id, token, client)
}

fn connect(engine: &tessera_core::Engine, url: &str, token: &str) -> Client {
    let key = tessera_server::load_identity(engine.paths()).unwrap();
    Client::connect(ServerConfig {
        endpoints: vec![url.to_string()],
        token: token.to_string(),
        server_key: tessera_secure::b64(key.public()),
        ..Default::default()
    })
}

#[tokio::test]
async fn note_pictures_go_only_to_paired_devices() {
    let (engine, url, _dir) = start().await;
    let (_, _, phone) = device(&engine, &url, "Pixel").await;
    let (desk_id, desk_token, desk) = device(&engine, &url, "Desktop").await;
    let mut photo = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    photo.extend(1024u32.to_be_bytes());
    photo.extend(768u32.to_be_bytes());
    photo.resize(1_500_000, 0x5a);
    let b64 = base64::engine::general_purpose::STANDARD;
    let a: NoteAsset = phone
        .call_as(Request::PutNoteAsset {
            mime: "image/png".into(),
            data: b64.encode(&photo),
        })
        .await
        .unwrap();
    assert_eq!((a.width, a.height, a.size), (1024, 768, photo.len() as u64));

    // The other device fetches it in pieces.
    let mut got = Vec::new();
    loop {
        let c: FileChunk = desk
            .call_as(Request::ReadNoteAsset {
                name: a.name.clone(),
                offset: got.len() as u64,
                len: 1 << 20,
            })
            .await
            .unwrap();
        got.extend(b64.decode(&c.data).unwrap());
        if c.eof {
            break;
        }
    }
    assert_eq!(got, photo);

    // No web address serves it, whatever it looks like (the web UI's own pages may answer).
    for path in [
        format!("/assets/{}", a.name),
        format!("/note-assets/{}", a.name),
        format!("/api/note-assets/{}", a.name),
        format!("/board-lib/../note-assets/{}", a.name),
    ] {
        let r = reqwest::get(format!("{url}{path}")).await.unwrap();
        let status = r.status();
        let body = r.bytes().await.unwrap();
        assert!(!body.starts_with(b"\x89PNG"), "{path}: {status}");
    }

    // A device that is removed can't connect to ask any more.
    engine
        .handle(Request::RevokeDevice { id: desk_id }, None)
        .await
        .unwrap();
    // Its connection that was already open stops at once too.
    let open = tokio::time::timeout(
        Duration::from_secs(3),
        desk.call_as::<FileChunk>(Request::ReadNoteAsset {
            name: a.name.clone(),
            offset: 0,
            len: 64,
        }),
    )
    .await;
    assert!(
        !matches!(open, Ok(Ok(_))),
        "a removed device's open connection still got the picture"
    );
    let again = connect(&engine, &url, &desk_token);
    assert!(
        again.wait_connected(Duration::from_secs(4)).await.is_err(),
        "a removed device connected again"
    );
    let asked = tokio::time::timeout(
        Duration::from_secs(3),
        again.call_as::<FileChunk>(Request::ReadNoteAsset {
            name: a.name.clone(),
            offset: 0,
            len: 64,
        }),
    )
    .await;
    assert!(
        !matches!(asked, Ok(Ok(_))),
        "a removed device got the picture"
    );
    drop(desk);
}
