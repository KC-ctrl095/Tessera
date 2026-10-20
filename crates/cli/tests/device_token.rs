//! Local token commands only, using an empty private home. No server,
//! service, user data or model is started. Bearer tokens never enter argv.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use tessera_core::{config::Paths, store::Store};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tessera"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("TESSERA_LOG", "off")
        .env_remove("TESSERA_HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn issue(home: &Path, keep_old: bool) -> (String, String) {
    let args: &[&str] = if keep_old {
        &["device-token", "--name", "desktop", "--keep-old"]
    } else {
        &["device-token", "--name", "desktop"]
    };
    let out = run(home, args);
    assert!(out.status.success(), "local issuance failed");
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).expect("local key JSON");
    let id = data["device_id"].as_str().unwrap().to_owned();
    let token = data["token"].as_str().unwrap().to_owned();
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains(&token),
        "stderr included a key"
    );
    (id, token)
}

#[test]
fn default_cli_still_replaces_earlier_same_name_logins() {
    let home = tempfile::tempdir().unwrap();
    let (_, old) = issue(home.path(), false);
    let (id, token) = issue(home.path(), false);
    let store = Store::open(&Paths::at(home.path().to_owned()).db_file()).unwrap();
    assert!(store.authenticate(&old).unwrap().is_none());
    assert!(store.authenticate(&token).unwrap().as_deref() == Some(id.as_str()));
}

#[test]
fn keep_old_is_safe_to_abandon_and_retirement_never_revokes_a_newer_issue() {
    let home = tempfile::tempdir().unwrap();
    let (_, old) = issue(home.path(), false);
    let (keep, token) = issue(home.path(), true);
    let (newer, next_token) = issue(home.path(), true);
    let store = Store::open(&Paths::at(home.path().to_owned()).db_file()).unwrap();
    // Closing or timing out after either issuance leaves the saved login valid.
    for t in [&old, &token, &next_token] {
        assert!(store.authenticate(t).unwrap().is_some());
    }
    let out = run(home.path(), &["device-token-retire", "--keep", &keep]);
    assert!(out.status.success(), "retirement failed");
    assert!(out.stdout.is_empty(), "retirement should not print a key");
    assert!(store.authenticate(&old).unwrap().is_none());
    assert!(store.authenticate(&token).unwrap().is_some());
    assert!(store.authenticate(&next_token).unwrap().as_deref() == Some(newer.as_str()));
    assert!(
        run(home.path(), &["device-token-retire", "--keep", &newer])
            .status
            .success()
    );
    let late = run(home.path(), &["device-token-retire", "--keep", &keep]);
    assert!(!late.status.success(), "stale cleanup must refuse");
    assert!(store.authenticate(&next_token).unwrap().is_some());
}

#[test]
fn mistaken_retirement_input_is_not_echoed_or_accepted() {
    let home = tempfile::tempdir().unwrap();
    let (_, token) = issue(home.path(), true);
    // A stand-in, not a real bearer token, to test the error's redaction.
    let out = run(
        home.path(),
        &["device-token-retire", "--keep", "tsr_mistaken-input"],
    );
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("tsr_mistaken-input"));
    let store = Store::open(&Paths::at(home.path().to_owned()).db_file()).unwrap();
    assert!(store.authenticate(&token).unwrap().is_some());
}
