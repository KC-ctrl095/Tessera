//! What the model is sent stays within what it can use: long reads stop
//! where the engine would cut them, and pictures no provider takes never
//! enter the conversation. Scripted mock model throughout.

mod common;

use base64::Engine as _;
use common::*;
use serde_json::json;
use tessera_proto::*;

/// The header of a PNG of `w`×`h` pixels (enough for the size checks).
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    b
}

fn read(path: &str) -> Reply {
    Reply::Tools(vec![("read".into(), json!({ "path": path }))])
}

#[tokio::test]
async fn a_long_read_stops_where_the_window_allows_and_says_how_to_go_on() {
    // A 100k-token window: results are kept to ~14 KB, so a read stops
    // before the engine would cut ~28 KB (and lose the middle).
    let h = harness(
        vec![read("big.txt"), Reply::Text("Read it.".into())],
        100_000,
    )
    .await;
    let lines: String = (1..=2_000)
        .map(|i| format!("line {i:05} of a long file with some words in it\n"))
        .collect();
    std::fs::write(h.work.join("big.txt"), &lines).unwrap();
    let s = h.session().await;
    h.send(&s.id, "read big.txt").await;
    h.settle(&s.id).await;
    let (_, err, text) = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(!err, "{text}");
    assert!(text.len() < 28_000, "{} bytes", text.len());
    assert!(
        text.contains("Continue with offset="),
        "{}",
        &text[text.len() - 200..]
    );
    assert!(!text.contains("Output truncated"), "the middle was lost");
    // Every line shown is whole and in order from the start.
    assert!(text.contains("\n1:line 00001 of a long file"));
}

#[tokio::test]
async fn pictures_no_provider_takes_stay_out_of_the_conversation() {
    let h = harness(
        vec![
            read("wide.png"),
            read("ok.png"),
            Reply::Text("Looked.".into()),
            Reply::Text("Saw the note.".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("wide.png"), png(9_000, 400)).unwrap();
    std::fs::write(h.work.join("ok.png"), png(1_600, 1_200)).unwrap();
    let s = h.session().await;
    h.send(&s.id, "look at both pictures").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let (_, err, text) = &results[0];
    assert!(*err, "{text}");
    assert!(
        text.contains("9000×400 pixels") && text.contains("smaller copy"),
        "{text}"
    );
    assert!(!results[1].1, "{}", results[1].2);
    // An attachment too big to show is kept as a file, with a note.
    let big = base64::engine::general_purpose::STANDARD.encode(png(12_000, 9_000));
    h.engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text("and this one")],
                delivery: Delivery::Auto,
                attachments: vec![Attachment {
                    name: "scan.png".into(),
                    mime: "image/png".into(),
                    data: big,
                }],
            },
            None,
        )
        .await
        .unwrap();
    h.settle(&s.id).await;
    let entries = h.entries(&s.id).await;
    let sent = entries
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EntryBody::User { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        !sent.iter().any(|p| matches!(p, ContentPart::Image { .. })),
        "the picture went in"
    );
    let note = plain_text(&sent);
    assert!(
        note.contains("saved at") && note.contains("Not shown to the model"),
        "{note}"
    );
}

/// Reasoning is replayed only under the system prompt it was written with:
/// when a setting or a new credential changes the prompt mid-chat, older
/// reasoning stops being sent (Claude refuses it), and the history is
/// append-only again from there.
#[tokio::test]
async fn a_prompt_changed_mid_chat_stops_replaying_older_reasoning() {
    let h = harness(
        ["one", "two", "three", "four"]
            .map(|t| Reply::Text(t.into()))
            .to_vec(),
        100_000,
    )
    .await;
    let s = h.session().await;
    for text in ["first", "second"] {
        h.send(&s.id, text).await;
        h.settle(&s.id).await;
    }
    // A credential saved now is named in the system prompt.
    h.engine
        .handle(
            Request::SetSecret {
                name: "LAB_TOKEN".into(),
                value: "s3cret-value-123456".into(),
            },
            None,
        )
        .await
        .unwrap();
    for text in ["third", "fourth"] {
        h.send(&s.id, text).await;
        h.settle(&s.id).await;
    }
    let reqs = h.mock.agent_requests();
    assert_eq!(reqs.len(), 4);
    let reasoned = |r: &serde_json::Value| {
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "assistant" && m.get("reasoning_content").is_some())
            .count()
    };
    assert_eq!(
        reasoned(&reqs[1]),
        1,
        "the first reply's reasoning is replayed"
    );
    assert_ne!(
        reqs[2]["messages"][0], reqs[1]["messages"][0],
        "the prompt changed"
    );
    assert_eq!(reasoned(&reqs[2]), 0);
    assert_eq!(reqs[3]["messages"][0], reqs[2]["messages"][0]);
    assert_eq!(reasoned(&reqs[3]), 1, "only what came after the change");
    let (a, b) = (
        reqs[2]["messages"].as_array().unwrap(),
        reqs[3]["messages"].as_array().unwrap(),
    );
    assert_eq!(&b[..a.len()], &a[..], "append-only again");
}

/// Trimming old tool output edits the history before the recent turns:
/// their reasoning no longer matches what it was written after, so it stops
/// being replayed too (Claude refuses it; the documented design).
#[tokio::test]
async fn trimming_old_output_also_stops_replaying_older_reasoning() {
    // Each output is cut to the window's share, so filling 40k takes a few dozen steps.
    let mut script: Vec<Reply> = (0..32)
        .map(|i| {
            Reply::Tools(vec![(
                "bash".into(),
                json!({ "command": format!("echo step{i}; python3 -c 'print(\"x\" * 7000)'") }),
            )])
        })
        .collect();
    script.push(Reply::Text("done".into()));
    let h = harness_agent(
        script,
        40_000,
        "[security]\nsandbox = 'off'\ndefault_permission = 'auto'\n",
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "run the steps").await;
    h.settle(&s.id).await;
    let notices: Vec<String> = h
        .entries(&s.id)
        .await
        .into_iter()
        .filter_map(|e| match e.body {
            EntryBody::Notice { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("older tool output was replaced")),
        "{notices:#?}"
    );
    let reqs = h.mock.agent_requests();
    let reasoned = |r: &serde_json::Value| {
        r["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "assistant" && m.get("reasoning_content").is_some())
            .count()
    };
    let trimmed = reqs
        .iter()
        .position(|r| r.to_string().contains("output pruned from context"))
        .expect("no request after the trim");
    assert!(reasoned(&reqs[trimmed - 1]) > 1, "replayed before the trim");
    assert_eq!(
        reasoned(&reqs[trimmed]),
        0,
        "older reasoning after the trim"
    );
}

/// One `read` takes several files: each shown as its own read would (tag,
/// numbered lines), sharing one read's size so a big file can't crowd out
/// the rest; a file that can't be read says so and the others still come;
/// the lines shown can be edited by tag right after.
#[tokio::test]
async fn one_read_takes_several_files_within_one_reads_size() {
    let h = harness(
        vec![
            Reply::Tools(vec![(
                "read".into(),
                json!({"paths": ["big.txt", "a.py", "gone.txt", "docs"]}),
            )]),
            Reply::Text("Read them.".into()),
        ],
        100_000,
    )
    .await;
    let lines: String = (1..=2_000)
        .map(|i| format!("line {i:05} of a long file with some words in it\n"))
        .collect();
    std::fs::write(h.work.join("big.txt"), &lines).unwrap();
    std::fs::write(h.work.join("a.py"), "x = 1\ny = 2\n").unwrap();
    std::fs::create_dir(h.work.join("docs")).unwrap();
    std::fs::write(h.work.join("docs/guide.md"), "# Guide\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "read them").await;
    h.settle(&s.id).await;
    let (_, err, text) = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(!err, "{text}");
    let tag = tessera_core::tools::files::file_tag("x = 1\ny = 2\n");
    assert!(
        text.contains(&format!("[a.py#{tag}] 2 lines\n1:x = 1\n2:y = 2")),
        "{text}"
    );
    assert!(text.contains("[big.txt#") && text.contains("Continue with offset="));
    assert!(text.contains("Cannot read gone.txt"), "{text}");
    assert!(text.contains("guide.md"), "{text}");
    // Within what one read may show (~28 KB at this window), not 4 times it.
    assert!(text.len() < 28_000, "{} bytes", text.len());

    // Several files that can't be read: an error.
    let h = harness(
        vec![
            Reply::Tools(vec![("read".into(), json!({"paths": ["x.txt", "y.txt"]}))]),
            Reply::Text("None there.".into()),
        ],
        100_000,
    )
    .await;
    let s = h.session().await;
    h.send(&s.id, "read them").await;
    h.settle(&s.id).await;
    let (_, err, text) = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(err, "{text}");
}

/// Lines shown by a read of several files count as seen: they can be
/// edited by tag and line right after.
#[tokio::test]
async fn lines_from_a_read_of_several_files_can_be_edited_by_line() {
    let tag = tessera_core::tools::files::file_tag("x = 1\ny = 2\n");
    let h = harness(
        vec![
            Reply::Tools(vec![("read".into(), json!({"paths": ["a.py", "b.py"]}))]),
            Reply::Tools(vec![(
                "edit".into(),
                json!({"path": "b.py", "tag": tag, "lines": [{"at": "2", "text": "y = 3"}]}),
            )]),
            Reply::Text("Changed.".into()),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("a.py"), "a = 0\n").unwrap();
    std::fs::write(h.work.join("b.py"), "x = 1\ny = 2\n").unwrap();
    let s = h.session().await;
    h.send(&s.id, "change y").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    assert!(!results[1].1, "{}", results[1].2);
    assert_eq!(
        std::fs::read_to_string(h.work.join("b.py")).unwrap(),
        "x = 1\ny = 3\n"
    );
}

/// A file a single read may not see can't ride along in a read of several:
/// the whole call is refused and names it.
#[tokio::test]
async fn a_read_of_several_files_is_refused_if_one_is_off_limits() {
    let h = harness(vec![], 100_000).await;
    std::fs::write(h.work.join("a.py"), "a = 0\n").unwrap();
    let config = h.dir.path().join("config.toml");
    h.mock.push(Reply::Tools(vec![(
        "read".into(),
        json!({"paths": ["a.py", config.display().to_string()]}),
    )]));
    h.mock.push(Reply::Text("Refused.".into()));
    let s = h.session().await;
    h.send(&s.id, "read them").await;
    h.settle(&s.id).await;
    let (_, err, text) = tool_results(&h.entries(&s.id).await).remove(0);
    assert!(err, "{text}");
    assert!(text.contains("config.toml: Refused"), "{text}");
    assert!(!text.contains("a = 0"), "{text}");
}

/// At a small tool output limit (4,320 bytes at a 30k window), a read of a
/// directory and 19 big files stays within what one read may show, headers
/// included, so the engine never cuts its middle; the files left out say so
/// and don't count as read (a `write` over one is refused), while a file
/// shown does.
#[tokio::test]
async fn a_read_of_many_files_stays_within_the_limit_and_marks_only_what_it_showed() {
    let mut paths = vec!["docs".to_string()];
    paths.extend((0..19).map(|i| format!("big{i:02}.txt")));
    let h = harness(
        vec![
            Reply::Tools(vec![("read".into(), json!({ "paths": paths }))]),
            Reply::Tools(vec![(
                "write".into(),
                json!({"path": "big05.txt", "content": "mine\n"}),
            )]),
            Reply::Tools(vec![(
                "edit".into(),
                json!({"path": "big05.txt", "tag": tessera_core::tools::files::file_tag(&long_file()), "lines": [{"at": "1", "text": "mine"}]}),
            )]),
            Reply::Tools(vec![(
                "write".into(),
                json!({"path": "big00.txt", "content": "mine\n"}),
            )]),
            Reply::Text("done".into()),
        ],
        30_000,
    )
    .await;
    let lines = long_file();
    for i in 0..19 {
        std::fs::write(h.work.join(format!("big{i:02}.txt")), &lines).unwrap();
    }
    std::fs::create_dir(h.work.join("docs")).unwrap();
    for i in 0..300 {
        std::fs::write(
            h.work
                .join("docs")
                .join(format!("a-rather-long-file-name-for-a-listing-{i:03}.md")),
            "x",
        )
        .unwrap();
    }
    let limit = tessera_core::context::tool_output_limit(
        tessera_core::config::ContextConfig::default().max_tool_output,
        30_000,
    );
    let s = h.session().await;
    h.send(&s.id, "read them").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let read = &results[0].2;
    assert!(!results[0].1, "{read}");
    assert!(
        read.len() <= 2 * limit - 1024,
        "{} bytes > {}",
        read.len(),
        2 * limit
    );
    assert!(!read.contains("Output truncated"), "{read}");
    assert!(
        read.contains("docs (300 entries)") && read.contains("more entries"),
        "{read}"
    );
    assert!(read.contains("[big00.txt#"), "{read}");
    assert!(
        read.contains("big18.txt: not read here (the result would be too long)"),
        "{read}"
    );
    // A file left out wasn't seen: overwriting it is refused. One shown was.
    assert!(
        results[1].1 && results[1].2.contains("no record of a read"),
        "{:?}",
        results[1]
    );
    assert!(results[2].1, "{:?}", results[2]);
    assert!(!results[3].1, "{:?}", results[3]);
}

fn long_file() -> String {
    (1..=2_000)
        .map(|i| format!("line {i:05} of a long file with some words in it\n"))
        .collect()
}

/// Headers and continuation lines also fit with the user's larger window.
#[tokio::test]
async fn twenty_large_files_include_headers_without_engine_truncation() {
    let paths: Vec<_> = (0..20)
        .map(|i| format!("long-file-name-{i:02}.txt"))
        .collect();
    let h = harness(
        vec![
            Reply::Tools(vec![("read".into(), json!({"paths": paths}))]),
            Reply::Text("done".into()),
        ],
        232_000,
    )
    .await;
    for path in &paths {
        std::fs::write(h.work.join(path), long_file()).unwrap();
    }
    let s = h.session().await;
    h.send(&s.id, "read them").await;
    h.settle(&s.id).await;
    let results = tool_results(&h.entries(&s.id).await);
    let text = &results[0].2;
    let limit = tessera_core::context::tool_output_limit(
        tessera_core::config::ContextConfig::default().max_tool_output,
        232_000,
    );
    assert!(text.len() <= 2 * limit - 1024, "{} bytes", text.len());
    assert!(!text.contains("Output truncated"));
    for path in paths {
        assert!(text.contains(&format!("[{path}#")), "missing {path}");
    }
}
