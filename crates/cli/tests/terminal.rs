//! The terminal app end to end: `tessera` in a real terminal (a private
//! tmux server) against a throwaway home and a scripted model. It opens a
//! chat, sends a message, shows the answer and the status line, lists the
//! chat in `/chats`, and the chat is a lean terminal chat (the model is
//! offered the coding tools only). Skipped where tmux isn't installed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A scripted OpenAI-style model: lists one model, names chats, and
/// answers a chat request with the reply whose key its last message holds.
struct Mock {
    port: u16,
    /// Tool names offered in each chat request.
    offered: Arc<Mutex<Vec<Vec<String>>>>,
}

fn start_mock(replies: Vec<(&'static str, &'static str)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let offered = Arc::new(Mutex::new(Vec::new()));
    let seen = offered.clone();
    let replies = Arc::new(replies);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let seen = seen.clone();
            let replies = replies.clone();
            std::thread::spawn(move || serve(stream, &seen, &replies));
        }
    });
    Mock { port, offered }
}

fn serve(
    mut stream: TcpStream,
    seen: &Mutex<Vec<Vec<String>>>,
    replies: &[(&'static str, &'static str)],
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    if first.starts_with("GET") {
        let json = r#"{"data":[{"id":"m","context_length":32768}]}"#;
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let system = req["messages"][0]["content"].as_str().unwrap_or("");
    let text = if system.starts_with("You name conversations") {
        "Greeting the mock".to_string()
    } else {
        let names = req["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        seen.lock().unwrap().push(names);
        let last = req["messages"]
            .as_array()
            .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
            .map(|m| m["content"].to_string())
            .unwrap_or_default();
        replies
            .iter()
            .find(|(k, _)| last.contains(k))
            .map_or("done", |(_, r)| r)
            .to_string()
    };
    let chunk = serde_json::json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1200, "completion_tokens": 8}});
    let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
        sse.len()
    );
}

struct Tmux {
    socket: String,
}

impl Tmux {
    fn run(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn screen(&self) -> String {
        self.run(&["capture-pane", "-p", "-t", "t"])
    }

    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", "t"];
        args.extend(keys);
        self.run(&args);
    }

    /// Wait until the screen shows `want` (or fail, showing the screen).
    fn wait_for(&self, want: &str) -> String {
        self.wait_for_any(&[want])
    }

    /// Wait until the screen no longer shows `gone`.
    fn wait_for_gone(&self, gone: &str) -> String {
        let start = Instant::now();
        loop {
            let s = self.screen();
            if !s.contains(gone) {
                return s;
            }
            assert!(
                start.elapsed() < Duration::from_secs(40),
                "still showed {gone:?}:\n{s}"
            );
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Wait until the screen shows one of `wants`.
    fn wait_for_any(&self, wants: &[&str]) -> String {
        let start = Instant::now();
        loop {
            let s = self.screen();
            if wants.iter().any(|w| s.contains(w)) {
                return s;
            }
            assert!(
                start.elapsed() < Duration::from_secs(40),
                "never showed {wants:?}:\n{s}"
            );
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

#[test]
fn the_terminal_opens_a_lean_chat_and_answers() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux isn't installed: skipped");
        return;
    }
    let mock = start_mock(vec![
        ("hello there", "Hello from the mock"),
        ("second question", "Second answer"),
    ]);
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            r#"default_model = "mock/m"
[agent]
review_interval = 0
[tuning."*"]
small = "off"
check = false
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "http://127.0.0.1:{}/v1"
discover = false
[[provider.models]]
id = "m"
name = "Mock model"
context_window = 32768
max_output = 2000
"#,
            mock.port
        ),
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_tessera");
    let tmux = Tmux {
        socket: format!("tessera-test-{}", std::process::id()),
    };
    // A prompt on the command line: no setup question, a new chat with it.
    let cmd = format!(
        "env TESSERA_HOME='{}' TESSERA_NO_ANIM=1 TERM=xterm-256color '{bin}' 'hello there'; echo EXITED; sleep 30",
        home.path().display()
    );
    tmux.run(&[
        "new-session",
        "-d",
        "-s",
        "t",
        "-x",
        "100",
        "-y",
        "30",
        "-c",
        &work.path().display().to_string(),
        &cmd,
    ]);
    let s = tmux.wait_for("Hello from the mock");
    // One chat across the window: your message, the answer, the message
    // box and the status line under it. No tiles, no list of chats.
    assert!(s.contains("▎ hello there"), "{s}");
    let s = tmux.wait_for("■ ready");
    let rows: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let status = rows.last().unwrap();
    assert!(status.contains("Mock model"), "{s}");
    assert!(rows[rows.len() - 2].trim_start().starts_with('╰'), "{s}");
    // The chat is a terminal chat: the coding tools only.
    let offered = mock.offered.lock().unwrap()[0].clone();
    for t in ["read", "edit", "bash", "grep"] {
        assert!(offered.iter().any(|n| n == t), "{t} in {offered:?}");
    }
    for t in [
        "note",
        "whiteboard",
        "browser",
        "memory",
        "message_bot",
        "schedule",
    ] {
        assert!(!offered.iter().any(|n| n == t), "{t} in {offered:?}");
    }
    // A second message from the keyboard.
    tmux.keys(&["second question", "Enter"]);
    tmux.wait_for("Second answer");
    // /chats lists it (with the title the mock gave it).
    tmux.keys(&["/chats", "Enter"]);
    tmux.wait_for("Your chats");
    tmux.wait_for("Greeting the mock");
    tmux.keys(&["Escape"]);
    // /split: a second chat beside it, each with its title; /close goes
    // back to one (the empty one isn't kept).
    tmux.keys(&["/split", "Enter"]);
    let s = tmux.wait_for("● New chat");
    let first = s.lines().next().unwrap_or_default();
    assert!(first.contains("○ Greeting the mock"), "{s}");
    assert!(s.lines().nth(8).is_some_and(|l| l.contains('│')), "{s}");
    tmux.keys(&["/close", "Enter"]);
    let s = tmux.wait_for_gone("● New chat");
    assert!(s.contains("Second answer"), "{s}");
    // /settings: the few things the terminal needs.
    tmux.keys(&["/settings", "Enter"]);
    let s = tmux.wait_for("Model for new chats");
    for want in ["Permissions", "Show thinking", "Theme", "Mouse"] {
        assert!(s.contains(want), "{want}: {s}");
    }
    tmux.keys(&["Escape"]);
    // /help, then quit with ctrl+c twice: the chat is kept.
    tmux.keys(&["?"]);
    tmux.wait_for("Keys and commands");
    tmux.keys(&["Escape"]);
    tmux.keys(&["C-c"]);
    tmux.keys(&["C-c"]);
    let s = tmux.wait_for("EXITED");
    assert!(s.contains("continue with tessera here"), "{s}");
    assert!(mock.offered.lock().unwrap().len() >= 2, "two answers");
}

#[test]
fn a_chat_from_the_apps_reopens_with_its_tools_and_says_so() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux isn't installed: skipped");
        return;
    }
    let mock = start_mock(vec![
        ("make it", "Made with every tool"),
        ("and now?", "Still here"),
    ]);
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"mock/m\"\n[agent]\nreview_interval = 0\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"mock\"\nname = \"Mock\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:{}/v1\"\ndiscover = false\n[[provider.models]]\nid = \"m\"\ncontext_window = 32768\n",
            mock.port
        ),
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_tessera");
    // A chat with every tool (as the apps make them), in this folder.
    let out = Command::new(bin)
        .args(["run", "--all-tools", "-q", "make it"])
        .env("TESSERA_HOME", home.path())
        .current_dir(work.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(mock.offered.lock().unwrap()[0].iter().any(|n| n == "note"));
    let tmux = Tmux {
        socket: format!("tessera-test-app-{}", std::process::id()),
    };
    let cmd = format!(
        "env TESSERA_HOME='{}' TESSERA_NO_ANIM=1 TERM=xterm-256color '{bin}'; sleep 30",
        home.path().display()
    );
    tmux.run(&[
        "new-session",
        "-d",
        "-s",
        "t",
        "-x",
        "100",
        "-y",
        "30",
        "-c",
        &work.path().display().to_string(),
        &cmd,
    ]);
    // `tessera` here reopens it; the first start offers the setup first
    // ("Don't ask again").
    let s = tmux.wait_for_any(&["Made with every tool", "Set up Tessera?"]);
    if s.contains("Set up Tessera?") {
        tmux.keys(&["Down", "Down", "Enter"]);
    }
    let s = tmux.wait_for("Made with every tool");
    assert!(s.contains("▎ make it"), "{s}");
    let s = tmux.wait_for("keeps all of its tools");
    assert!(s.contains("started in the apps"), "{s}");
    // It still answers here, with its own tools.
    tmux.keys(&["and now?", "Enter"]);
    tmux.wait_for("Still here");
    let offered = mock.offered.lock().unwrap();
    assert!(offered.last().unwrap().iter().any(|n| n == "note"));
}
