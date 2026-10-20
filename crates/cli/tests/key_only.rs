//! The desktop app asks for this computer's server key to know a saved
//! server is this computer's own (it then starts it when it isn't running):
//! `device-token --key-only` says it without making a key or a device.

use std::process::{Command, Stdio};

fn tessera(home: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tessera"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env("XDG_CACHE_HOME", home.join("xdg-cache"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn the_key_is_said_without_making_one_or_a_device() {
    let home = tempfile::tempdir().unwrap();
    // No server here yet: nothing to say, and nothing made.
    let (code, out) = tessera(home.path(), &["device-token", "--key-only"]);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.trim().is_empty());
    assert!(!home.path().join("identity.key").exists());
    // Once there is one (an app's key made it), the same key every time.
    let (code, issued) = tessera(home.path(), &["device-token", "--name", "Desktop app"]);
    assert_eq!(code, Some(0));
    let issued: serde_json::Value = serde_json::from_str(issued.trim()).unwrap();
    let (code, out) = tessera(home.path(), &["device-token", "--key-only"]);
    assert_eq!(code, Some(0));
    let key: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(key["server_key"], issued["server_key"]);
    // Only the key: no device was added.
    assert!(key.get("token").is_none() && key.get("device_id").is_none());
    // One it can't read is left as it is, never replaced.
    std::fs::write(home.path().join("identity.key"), "not a key\n").unwrap();
    let (code, out) = tessera(home.path(), &["device-token", "--key-only"]);
    assert_eq!(code, Some(3), "{out}");
    assert_eq!(
        std::fs::read_to_string(home.path().join("identity.key")).unwrap(),
        "not a key\n"
    );
}
