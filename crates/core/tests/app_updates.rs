//! App builds the server offers its devices (`tessera updates add`,
//! `get_app_update`, `read_app_update`): which device is offered which
//! build, the download, and nothing outside the updates folder.

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tessera_core::config::Paths;
use tessera_core::updates::Updates;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

/// A pretend Android package for 64-bit ARM phones: a zip with native code.
fn apk(tag: &str) -> Vec<u8> {
    let mut b = b"PK\x03\x04".to_vec();
    b.extend_from_slice(format!("build {tag} ").repeat(50_000).as_bytes());
    b.extend_from_slice(b"lib/arm64-v8a/libtessera_app_lib.so PK\x01\x02");
    b
}

/// A pretend Linux program for x86_64.
fn elf() -> Vec<u8> {
    let mut b = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0".to_vec();
    b.extend_from_slice(&62u16.to_le_bytes());
    b.extend_from_slice(&[7u8; 4000]);
    b
}

/// A synthetic NSIS header (not executable) for validation/download tests.
fn nsis(machine: u16) -> Vec<u8> {
    let mut b = vec![0; 4000];
    b[..2].copy_from_slice(b"MZ");
    b[60..64].copy_from_slice(&128u32.to_le_bytes());
    b[128..132].copy_from_slice(b"PE\0\0");
    b[132..134].copy_from_slice(&machine.to_le_bytes());
    b[148..150].copy_from_slice(&224u16.to_le_bytes());
    b[150..152].copy_from_slice(&2u16.to_le_bytes());
    b[152..154].copy_from_slice(
        &(if machine == 0x8664 {
            0x20bu16
        } else {
            0x10bu16
        })
        .to_le_bytes(),
    );
    b[1024..1040].copy_from_slice(b"\xef\xbe\xad\xdeNullsoftInst");
    b
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

async fn engine(dir: &std::path::Path) -> Engine {
    Engine::open(Paths::at(dir), EngineOptions { scheduler: false })
        .await
        .unwrap()
}

async fn offer(
    e: &Engine,
    platform: &str,
    version: &str,
    sha256: Option<&str>,
    installed_at: Option<u64>,
) -> Result<Option<AppUpdate>, ApiError> {
    e.handle(
        Request::GetAppUpdate {
            platform: platform.into(),
            version: version.into(),
            sha256: sha256.map(str::to_string),
            installed_at,
        },
        None,
    )
    .await
    .map(|v| serde_json::from_value(v).unwrap())
}

async fn download(e: &Engine, id: &str) -> Vec<u8> {
    let mut got = Vec::new();
    loop {
        let v = e
            .handle(
                Request::ReadAppUpdate {
                    id: id.into(),
                    offset: got.len() as u64,
                    len: 64 * 1024,
                },
                None,
            )
            .await
            .unwrap();
        let c: FileChunk = serde_json::from_value(v).unwrap();
        got.extend(
            base64::engine::general_purpose::STANDARD
                .decode(c.data)
                .unwrap(),
        );
        if c.eof {
            return got;
        }
    }
}

#[tokio::test]
async fn devices_are_offered_newer_builds_only() {
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path()).await;
    let updates = Updates::new(dir.path());

    // Nothing offered yet.
    assert_eq!(
        offer(&e, "android-arm64", "0.1.0", None, None).await,
        Ok(None)
    );

    let first = apk("one");
    let f1 = dir.path().join("tessera-android-arm64.apk");
    std::fs::write(&f1, &first).unwrap();
    let a = updates
        .add(&f1, None, Some("0.1.0"), Some("First one"))
        .unwrap();
    assert_eq!(a.platform, "android-arm64", "found from the file");
    assert_eq!(a.sha256, sha(&first));
    assert_eq!(a.size, first.len() as u64);

    // The phone runs exactly that build: up to date.
    assert_eq!(
        offer(&e, "android-arm64", "0.1.0", Some(&a.sha256), Some(1)).await,
        Ok(None)
    );
    // An app of the same version installed before the build was added: offered.
    let got = offer(
        &e,
        "android-arm64",
        "0.1.0",
        Some("ff00"),
        Some(a.added_at - 1),
    )
    .await
    .unwrap()
    .expect("offered");
    assert_eq!(got.id, a.id);
    assert_eq!(got.notes.as_deref(), Some("First one"));
    // Installed after it (a build of its own, maybe newer): not offered.
    assert_eq!(
        offer(
            &e,
            "android-arm64",
            "0.1.0",
            Some("ff00"),
            Some(a.added_at + 1)
        )
        .await,
        Ok(None)
    );
    // Not knowing either: not offered.
    assert_eq!(
        offer(&e, "android-arm64", "0.1.0", None, None).await,
        Ok(None)
    );
    // An older version: offered; a newer one: never "updated" back.
    assert!(
        offer(&e, "android-arm64", "0.0.9", None, None)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        offer(&e, "android-arm64", "0.2.0", None, None).await,
        Ok(None)
    );
    // Other platforms have nothing.
    assert_eq!(
        offer(&e, "linux-x86_64-desktop", "0.0.1", None, None).await,
        Ok(None)
    );
    // An unknown platform is refused.
    let err = offer(&e, "windows-x86_64", "0.1.0", None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);

    // The download is the file, byte for byte.
    let bytes = download(&e, &a.id).await;
    assert_eq!(sha(&bytes), a.sha256);

    // A newer build replaces it: phones on the first one are offered the
    // second, whenever they were installed; the first file is gone.
    let second = apk("two");
    std::fs::write(&f1, &second).unwrap();
    let b = updates.add(&f1, None, Some("0.1.0"), None).unwrap();
    assert_ne!(a.id, b.id);
    let got = offer(
        &e,
        "android-arm64",
        "0.1.0",
        Some(&a.sha256),
        Some(u64::MAX),
    )
    .await
    .unwrap()
    .expect("an older build of ours");
    assert_eq!(got.id, b.id);
    assert_eq!(updates.list().unwrap().len(), 1, "one build per platform");
    assert!(!dir.path().join("updates").join(&a.file).exists());
    let gone = e
        .handle(
            Request::ReadAppUpdate {
                id: a.id.clone(),
                offset: 0,
                len: 10,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);

    // The same file again changes only the words.
    let again = updates
        .add(&f1, None, Some("0.1.0"), Some("Second, explained"))
        .unwrap();
    assert_eq!(again.id, b.id);
    assert_eq!(again.notes.as_deref(), Some("Second, explained"));

    // Removed: nothing offered.
    assert!(updates.remove("android-arm64").unwrap().is_some());
    assert_eq!(
        offer(&e, "android-arm64", "0.0.1", None, None).await,
        Ok(None)
    );
}

#[tokio::test]
async fn only_real_builds_for_known_platforms_are_added() {
    let dir = tempfile::tempdir().unwrap();
    let updates = Updates::new(dir.path());
    let f = dir.path().join("tessera-desktop");
    std::fs::write(&f, elf()).unwrap();
    // A program isn't an Android app, and the other way round.
    assert!(updates.add(&f, Some("android-arm64"), None, None).is_err());
    assert!(
        updates
            .add(&f, Some("linux-aarch64-desktop"), None, None)
            .is_err()
    );
    assert!(updates.add(&f, Some("macos-desktop"), None, None).is_err());
    assert!(updates.add(&f, None, Some("../1"), None).is_err());
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "hello").unwrap();
    assert!(updates.add(&text, None, None, None).is_err());
    let empty = dir.path().join("empty.apk");
    std::fs::write(&empty, "").unwrap();
    assert!(updates.add(&empty, None, None, None).is_err());
    // A program whose name doesn't say which one it is: asked to say it.
    let odd = dir.path().join("build-output");
    std::fs::write(&odd, elf()).unwrap();
    let e = updates.add(&odd, None, None, None).unwrap_err().to_string();
    assert!(e.contains("--platform"), "{e}");
    let ok = updates
        .add(&odd, Some("linux-x86_64-cli"), None, None)
        .unwrap();
    assert_eq!(
        ok.version,
        env!("CARGO_PKG_VERSION"),
        "the server's own by default"
    );
    let d = updates.add(&f, None, None, None).unwrap();
    assert_eq!(d.platform, "linux-x86_64-desktop");
    assert_eq!(updates.list().unwrap().len(), 2);
}

#[tokio::test]
async fn downloads_never_leave_the_updates_folder() {
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path()).await;
    let updates = Updates::new(dir.path());
    let f = dir.path().join("tessera");
    std::fs::write(&f, elf()).unwrap();
    let b = updates.add(&f, None, None, None).unwrap();
    std::fs::write(dir.path().join("secret.txt"), "not for devices").unwrap();
    let manifest = dir.path().join("updates/manifest.json");
    let read = |id: &str, offset: u64| {
        e.handle(
            Request::ReadAppUpdate {
                id: id.into(),
                offset,
                len: 1 << 20,
            },
            None,
        )
    };
    assert_eq!(read("nope", 0).await.unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(
        read(&b.id, 1 << 40).await.unwrap_err().code,
        ErrorCode::BadRequest
    );

    // A manifest edited to point elsewhere.
    let original = std::fs::read_to_string(&manifest).unwrap();
    for bad in ["../secret.txt", "/etc/passwd", "..", ".hidden"] {
        let v: Value = serde_json::from_str(&original).unwrap();
        let mut v = v;
        v["builds"][0]["file"] = Value::String(bad.into());
        std::fs::write(&manifest, serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!(
            read(&b.id, 0).await.unwrap_err().code,
            ErrorCode::NotFound,
            "{bad}"
        );
    }
    std::fs::write(&manifest, &original).unwrap();

    // The build swapped for a link to another file, or for other bytes.
    let path = dir.path().join("updates").join(&b.file);
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), &path).unwrap();
        assert_eq!(read(&b.id, 0).await.unwrap_err().code, ErrorCode::NotFound);
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::write(&path, b"something else").unwrap();
    assert_eq!(read(&b.id, 0).await.unwrap_err().code, ErrorCode::NotFound);
}

#[tokio::test]
async fn windows_nsis_is_offered_downloaded_and_checked_like_linux() {
    let dir = tempfile::tempdir().unwrap();
    let e = engine(dir.path()).await;
    let updates = Updates::new(dir.path());
    let file = dir.path().join("tessera-desktop-windows-x86_64-setup.exe");
    // NSIS uses a 32-bit stub for x64 payloads too.
    for machine in [0x14c, 0x8664] {
        let bytes = nsis(machine);
        std::fs::write(&file, &bytes).unwrap();
        let b = updates.add(&file, None, Some("0.2.0"), None).unwrap();
        assert_eq!(b.platform, "windows-x86_64-desktop");
        assert!(b.file.ends_with(".exe"));
        assert_eq!(
            offer(&e, &b.platform, "0.1.0", None, None)
                .await
                .unwrap()
                .unwrap()
                .id,
            b.id
        );
        assert!(
            offer(&e, &b.platform, "0.3.0", None, None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            offer(&e, &b.platform, "0.2.0", Some(&b.sha256), None)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(download(&e, &b.id).await, bytes);
        assert!(
            updates
                .add(&file, Some("linux-x86_64-desktop"), None, None)
                .is_err()
        );
    }
    // A DOS stub, portable PE, DLL, bad offset or ARM64 installer is refused.
    for bad in [
        b"MZnot a PE".to_vec(),
        {
            let mut b = nsis(0x14c);
            b[1024] = 0;
            b
        },
        {
            let mut b = nsis(0x14c);
            b[150..152].copy_from_slice(&0x2002u16.to_le_bytes());
            b
        },
        {
            let mut b = nsis(0x14c);
            b[60..64].fill(0xff);
            b
        },
        nsis(0xaa64),
    ] {
        std::fs::write(&file, &bad).unwrap();
        assert!(
            updates
                .add(&file, Some("windows-x86_64-desktop"), None, None)
                .is_err()
        );
    }
}

#[test]
fn old_and_new_clients_speak_the_same_request() {
    // A client that doesn't know its own build sends only platform and version.
    let r: ClientMessage = serde_json::from_str(
        r#"{"id":1,"method":"get_app_update","params":{"platform":"android-arm64","version":"0.1.0"}}"#,
    )
    .unwrap();
    assert!(matches!(
        r.request,
        Request::GetAppUpdate {
            sha256: None,
            installed_at: None,
            ..
        }
    ));
    // Nothing optional is sent when absent.
    let v = serde_json::to_value(Request::GetAppUpdate {
        platform: "android-arm64".into(),
        version: "0.1.0".into(),
        sha256: None,
        installed_at: None,
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"method":"get_app_update","params":{"platform":"android-arm64","version":"0.1.0"}})
    );
}
