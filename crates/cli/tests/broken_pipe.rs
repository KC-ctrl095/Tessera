//! No server or real user data is needed to check a closed CLI output pipe.
use std::process::{Command, Stdio};

#[test]
fn closed_stdout_exits_quietly() {
    let home = tempfile::tempdir().unwrap();
    for command in ["doctor", "sessions", "machines", "model"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tessera"))
            .args(["--local", "--home"])
            .arg(home.path())
            .args([command, "--plain"])
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Close before the first write, rather than depending on pipe capacity
        // or a race between the child and `head`.
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("panicked"), "{command}: {stderr}");
        assert!(!stderr.contains("Broken pipe"), "{command}: {stderr}");
    }
}
