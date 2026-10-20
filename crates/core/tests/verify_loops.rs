//! Batch 14 harness middleware, end to end with the scripted mock model:
//! the note when code changed and nothing checked it, the cut of a reply
//! that repeats itself, the nudge after "I'll run the tests" with no call,
//! near-duplicate loops and per-file edit counts, edit errors that show the
//! closest block, "already applied", and `apply_patch` for GPT models.

mod common;

use common::*;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

fn text(t: &str) -> Reply {
    Reply::Raw(vec![
        json!({"choices": [{"index": 0, "delta": {"content": t}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 10}}),
    ])
}

/// A reply streamed in small pieces.
fn streamed(t: &str) -> Reply {
    let mut chunks: Vec<Value> = Vec::new();
    let mut rest = t;
    while !rest.is_empty() {
        let mut n = 24.min(rest.len());
        while !rest.is_char_boundary(n) {
            n += 1;
        }
        chunks.push(json!({"choices": [{"index": 0, "delta": {"content": &rest[..n]}}]}));
        rest = &rest[n..];
    }
    chunks.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 400}}));
    Reply::Raw(chunks)
}

fn tool(name: &str, args: Value) -> Reply {
    Reply::Tools(vec![(name.into(), args)])
}

fn harness_notes(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::User {
                content,
                author: Some(a),
            } if a == "harness" => Some(plain_text(content)),
            _ => None,
        })
        .collect()
}

fn notices(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn assistant_texts(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|e| match &e.body {
            EntryBody::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        AssistantPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
        .collect()
}

async fn run(h: &Harness, prompt: &str) -> Vec<Entry> {
    let s = h.session().await;
    h.send(&s.id, prompt).await;
    h.settle(&s.id).await;
    h.entries(&s.id).await
}

const CHECK_NOTE: &str = "[Check] You changed";

// ---- verification -----------------------------------------------------------

#[tokio::test]
async fn changed_code_with_nothing_run_gets_one_note() {
    // Ruby: no syntax checker and no language server here, so only a
    // command can check it.
    let h = harness(
        vec![
            tool("write", json!({"path": "app.rb", "content": "puts 'hi'\n"})),
            text("Done: app.rb prints hi."),
            // The note's extra step: a reason, then the turn ends.
            text("No check applies: there is no Ruby here to run it."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "write app.rb that prints hi").await;
    let notes = harness_notes(&entries);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(
        notes[0],
        "[Check] You changed app.rb since the last check. Run a check (tests, build or the program) or say briefly why none applies."
    );
    assert_eq!(h.mock.agent_requests().len(), 3);
    assert_eq!(
        assistant_texts(&entries).last().unwrap(),
        "No check applies: there is no Ruby here to run it."
    );
}

#[tokio::test]
async fn a_check_after_the_change_means_no_note() {
    let h = harness(
        vec![
            tool("write", json!({"path": "app.rb", "content": "puts 'hi'\n"})),
            tool("bash", json!({"command": "make test || true"})),
            text("Done, and checked."),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("Makefile"), "test:\n\t@echo ok\n").unwrap();
    let entries = run(&h, "write app.rb").await;
    assert!(
        harness_notes(&entries).is_empty(),
        "{:?}",
        harness_notes(&entries)
    );
    assert_eq!(h.mock.agent_requests().len(), 3);
}

#[tokio::test]
async fn told_not_to_run_anything_or_docs_only_means_no_note() {
    let h = harness(
        vec![
            tool("write", json!({"path": "app.rb", "content": "puts 1\n"})),
            text("Written."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "Write app.rb but don't run anything.").await;
    assert!(harness_notes(&entries).is_empty());

    let h = harness(
        vec![
            tool("write", json!({"path": "NOTES.md", "content": "# Notes\n"})),
            text("Written."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "write the notes").await;
    assert!(harness_notes(&entries).is_empty());
}

#[tokio::test]
async fn a_clean_syntax_check_counts_unless_strict() {
    if tessera_core::lsp::servers::which("python3").is_none() {
        eprintln!("no python3: skipped");
        return;
    }
    // `on`: the quick check of a.py was clean, which counts as a check.
    let script = || {
        vec![
            tool(
                "write",
                json!({"path": "a.py", "content": "def add(a, b):\n    return a + b\n"}),
            ),
            text("Added add()."),
            text("It's a two-line function; I compiled it."),
        ]
    };
    let h = harness_agent(script(), 100_000, "[lsp]\nenabled = false").await;
    let entries = run(&h, "write add").await;
    assert!(
        harness_notes(&entries).is_empty(),
        "{:?}",
        harness_notes(&entries)
    );
    // `strict`: only commands count.
    let h = harness_agent(
        script(),
        100_000,
        "verify_check = \"strict\"\n[lsp]\nenabled = false",
    )
    .await;
    let entries = run(&h, "write add").await;
    let notes = harness_notes(&entries);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("[Check] You changed a.py"));
    // `off`: never.
    let h = harness_agent(script(), 100_000, "verify_check = \"off\"").await;
    assert!(harness_notes(&run(&h, "write add").await).is_empty());
}

#[tokio::test]
async fn a_broken_python_file_gets_a_syntax_warning() {
    if tessera_core::lsp::servers::which("python3").is_none() {
        return;
    }
    let h = harness_agent(
        vec![
            tool(
                "write",
                json!({"path": "b.py", "content": "def f(:\n    pass\n"}),
            ),
            text("Oops."),
            text("I'll leave it."),
        ],
        100_000,
        "[lsp]\nenabled = false",
    )
    .await;
    let entries = run(&h, "write b.py").await;
    let r = tool_results(&entries);
    assert!(
        r[0].2
            .contains("Warning: b.py has a syntax error (python): invalid syntax")
            || r[0]
                .2
                .contains("Warning: b.py has a syntax error (python):"),
        "{r:?}"
    );
    // Broken is not checked: the note still comes.
    assert_eq!(harness_notes(&entries).len(), 1);
}

#[tokio::test]
async fn shell_scripts_are_syntax_checked_and_say_when_they_broke() {
    let h = harness_agent(
        vec![
            tool(
                "write",
                json!({"path": "run.sh", "content": "if true; then\n  echo ok\nfi\n"}),
            ),
            tool(
                "edit",
                json!({"path": "run.sh", "replace": [{"old": "fi\n", "new": ""}]}),
            ),
            text("Edited."),
            text("Can't tell."),
        ],
        100_000,
        "[lsp]\nenabled = false",
    )
    .await;
    let entries = run(&h, "edit run.sh").await;
    let r = tool_results(&entries);
    assert!(!r[0].2.contains("Warning"), "{r:?}");
    assert!(
        r[1].2
            .contains("Warning: run.sh has a syntax error now (bash -n): line"),
        "{}",
        r[1].2
    );
}

#[tokio::test]
async fn the_check_note_takes_the_place_of_the_completion_check() {
    let h = harness(
        vec![
            tool(
                "task",
                json!({"goal": "app", "done_criteria": ["it runs"], "plan": [{"text": "write", "status": "pending"}]}),
            ),
            tool("write", json!({"path": "app.rb", "content": "puts 1\n"})),
            text("Done."),
            text("Can't run Ruby here."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "make the app").await;
    let notes = harness_notes(&entries);
    assert_eq!(notes.len(), 1, "one end-of-turn note: {notes:?}");
    assert!(notes[0].starts_with(CHECK_NOTE));
    assert_eq!(h.mock.agent_requests().len(), 4);
}

// ---- repetition while streaming ------------------------------------------------

#[tokio::test]
async fn a_reply_that_repeats_itself_is_cut_and_nudged_once() {
    let round = "The configuration file is missing the entry for the linker path. ";
    let looping = format!(
        "The build fails at the linker step.\n\n{}",
        round.repeat(30)
    );
    let h = harness(
        vec![
            streamed(&looping),
            text("The linker needs libssl; install libssl-dev."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "why does the build fail?").await;
    let texts = assistant_texts(&entries);
    assert_eq!(
        texts[0],
        format!(
            "The build fails at the linker step.\n\n{}",
            round.trim_end()
        ),
        "kept the useful part"
    );
    let notes = harness_notes(&entries);
    assert_eq!(
        notes,
        vec![
            "[Repetition] Your reply started repeating. Continue from where it was still useful, briefly."
        ]
    );
    assert_eq!(
        texts.last().unwrap(),
        "The linker needs libssl; install libssl-dev."
    );

    // Twice: one nudge, then a visible notice and the turn ends.
    let h = harness(vec![streamed(&looping), streamed(&looping)], 100_000).await;
    let entries = run(&h, "why?").await;
    assert_eq!(harness_notes(&entries).len(), 1);
    assert!(
        notices(&entries)
            .iter()
            .any(|n| n.contains("kept repeating itself")),
        "{:?}",
        notices(&entries)
    );
    assert_eq!(h.mock.agent_requests().len(), 2);

    // Off: the reply stays as it came.
    let h = harness_agent(vec![streamed(&looping)], 100_000, "repetition_cut = false").await;
    let entries = run(&h, "why?").await;
    assert_eq!(assistant_texts(&entries)[0], looping);
    assert!(harness_notes(&entries).is_empty());
}

#[tokio::test]
async fn a_long_list_is_not_a_loop() {
    let list: String = (0..40)
        .map(|i| format!("- crates/core/src/engine/some/long/shared/prefix/module_{i}.rs\n"))
        .collect();
    let h = harness(vec![streamed(&list)], 100_000).await;
    let entries = run(&h, "list the modules").await;
    assert_eq!(assistant_texts(&entries)[0], list);
    assert!(harness_notes(&entries).is_empty());
}

// ---- "I'll run the tests now." and no call ----------------------------------------

#[tokio::test]
async fn an_announced_action_without_a_call_gets_one_note() {
    let h = harness(
        vec![
            text("I found the bug in parse(). Let me fix it now."),
            tool("bash", json!({"command": "echo fixed"})),
            text("Let me run the tests."),
            text("Fixed."),
        ],
        100_000,
    )
    .await;
    let entries = run(&h, "fix parse").await;
    let notes = harness_notes(&entries);
    assert_eq!(
        notes,
        vec![
            "[No tool was called. If you meant to act, call the tool now; if you are finished, give the final answer.]"
        ],
        "once per turn"
    );
    // The second announcement ended the turn.
    assert_eq!(
        assistant_texts(&entries).last().unwrap(),
        "Let me run the tests."
    );

    // Off, and ordinary endings: nothing.
    let h = harness_agent(
        vec![text("Let me fix it now.")],
        100_000,
        "action_nudge = false",
    )
    .await;
    assert!(harness_notes(&run(&h, "fix").await).is_empty());
    let h = harness(vec![text("Done. Let me know if you need more.")], 100_000).await;
    assert!(harness_notes(&run(&h, "fix").await).is_empty());
}

// ---- loops -----------------------------------------------------------------------

#[tokio::test]
async fn near_duplicate_failing_commands_are_one_loop() {
    // Exit codes survive `2>&1`; a pipe to `tail` would hide them.
    let variants = [
        "exit 3",
        "exit 3 2>&1",
        "exit  3",
        "exit 3;",
        "  exit 3 ",
        "exit 3 2>&1;",
    ];
    let mut script: Vec<Reply> = variants
        .iter()
        .map(|c| tool("bash", json!({"command": c})))
        .collect();
    script.push(text("Stopped."));
    let h = harness(script, 100_000).await;
    let entries = run(&h, "try it").await;
    let r = tool_results(&entries);
    assert!(
        r[1].2
            .contains("[Loop warning: this call has failed 2 times"),
        "{r:?}"
    );
    assert!(r[5].2.starts_with("Blocked"), "{r:?}");
}

#[tokio::test]
async fn one_file_edited_again_and_again_gets_a_note() {
    let mut script = vec![tool(
        "write",
        json!({"path": "app.rb", "content": "x = 0\n"}),
    )];
    for i in 1..=5 {
        script.push(tool(
            "edit",
            json!({"path": "app.rb", "replace": [{"old": format!("x = {}", i - 1), "new": format!("x = {i}")}]}),
        ));
    }
    script.push(text("Done."));
    script.push(text("No Ruby to run."));
    let h = harness(script, 100_000).await;
    let entries = run(&h, "count up").await;
    let r = tool_results(&entries);
    let noted: Vec<usize> = r
        .iter()
        .enumerate()
        .filter(|(_, x)| x.2.contains("change to app.rb with nothing run since"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(noted, vec![4], "the 5th change, once: {r:?}");
}

// ---- edit errors ---------------------------------------------------------------------

#[tokio::test]
async fn no_match_shows_the_closest_block_and_a_repeat_is_already_applied() {
    let src =
        "def total(items):\n    s = 0\n    for i in items:\n        s += i.price\n    return s\n";
    let h = harness(
        vec![
            // Slightly wrong old text.
            tool(
                "edit",
                json!({"path": "shop.rb", "replace": [{"old": "    for item in items:\n        s += item.price", "new": "    s = sum(items)"}]}),
            ),
            tool(
                "edit",
                json!({"path": "shop.rb", "replace": [{"old": "    s = 0\n    for i in items:\n        s += i.price\n    return s", "new": "    return sum(i.price for i in items)"}]}),
            ),
            // Sent again.
            tool(
                "edit",
                json!({"path": "shop.rb", "replace": [{"old": "    s = 0\n    for i in items:\n        s += i.price\n    return s", "new": "    return sum(i.price for i in items)"}]}),
            ),
            text("Done."),
            text("Fine."),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("shop.rb"), src).unwrap();
    let entries = run(&h, "simplify total").await;
    let r = tool_results(&entries);
    assert!(r[0].1, "{r:?}");
    assert!(
        r[0].2.contains("The closest block is lines 3-4 (")
            && r[0]
                .2
                .contains("% similar):\n3:    for i in items:\n4:        s += i.price\n"),
        "{}",
        r[0].2
    );
    assert!(!r[1].1, "{r:?}");
    assert!(!r[2].1, "already applied is not an error: {r:?}");
    assert!(
        r[2].2
            .starts_with("No change: shop.rb already has this edit"),
        "{}",
        r[2].2
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("shop.rb")).unwrap(),
        "def total(items):\n    return sum(i.price for i in items)\n"
    );
}

#[tokio::test]
async fn several_matches_answer_with_the_working_call_and_the_loop_warning_repeats_it() {
    // The qwen baseline (bench git-branch-commit): the same edit four times.
    let call = json!({"path": "README.md", "replace": [{"new": "receive", "old": "recieve"}], "tag": "AF1B"});
    let fix = json!({"path": "README.md", "replace": [{"old": "recieve", "new": "receive", "all": true}]});
    let h = harness(
        vec![
            tool("edit", call.clone()),
            tool("edit", call.clone()),
            // `all` said for the whole call is taken too.
            tool(
                "edit",
                json!({"path": "README.md", "all": true, "replace": [{"new": "receive", "old": "recieve"}]}),
            ),
            text("Fixed both."),
        ],
        100_000,
    )
    .await;
    std::fs::write(
        h.work.join("README.md"),
        "We recieve mail.\nThey recieve it too.\n",
    )
    .unwrap();
    let entries = run(&h, "fix the typo everywhere").await;
    let r = tool_results(&entries);
    assert!(r[0].1, "{r:?}");
    assert!(
        r[0].2.ends_with(&format!("To change all 2, send: {fix}")),
        "{}",
        r[0].2
    );
    assert!(
        r[1].2.contains(&format!(
            "[Loop warning: this call has failed 2 times the same way. Send this instead: {fix}]"
        )),
        "{}",
        r[1].2
    );
    assert!(!r[2].1, "{r:?}");
    assert_eq!(
        std::fs::read_to_string(h.work.join("README.md")).unwrap(),
        "We receive mail.\nThey receive it too.\n"
    );
}

#[tokio::test]
async fn line_edits_without_a_tag_use_the_read_of_the_current_file() {
    let h = harness(
        vec![
            tool("read", json!({"path": "Makefile"})),
            // No tag (bench makefile-tabs), and a sed-style range.
            tool(
                "edit",
                json!({"path": "Makefile", "lines": [{"at": "2,2", "text": "\techo built"}]}),
            ),
            text("Done."),
            text("No make here."),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("Makefile"), "all:\n    echo built\n").unwrap();
    let entries = run(&h, "fix the tab").await;
    let r = tool_results(&entries);
    assert!(!r[1].1, "{r:?}");
    assert!(r[1].2.contains("(No `tag` given: used #"), "{}", r[1].2);
    assert_eq!(
        std::fs::read_to_string(h.work.join("Makefile")).unwrap(),
        "all:\n\techo built\n"
    );

    // Changed since the read: refused, as before.
    let h = harness(
        vec![
            tool("read", json!({"path": "a.txt"})),
            tool("bash", json!({"command": "echo more >> a.txt"})),
            tool(
                "edit",
                json!({"path": "a.txt", "lines": [{"at": "1", "text": "x"}]}),
            ),
            text("Hm."),
        ],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("a.txt"), "one\n").unwrap();
    let r = tool_results(&run(&h, "edit").await);
    assert!(r[2].1 && r[2].2.contains("need the `tag`"), "{r:?}");
}

#[tokio::test]
async fn git_merges_take_their_default_message() {
    let id = "-c user.email=a@example.com -c user.name=a";
    let script = format!(
        "git init -q r && cd r && git {id} commit -q --allow-empty -m one && git checkout -q -b side && \
         git {id} commit -q --allow-empty -m two && git checkout -q - && git {id} commit -q --allow-empty -m three && \
         git {id} merge -q --no-ff side && git log -1 --format=%s"
    );
    let h = harness(
        vec![tool("bash", json!({"command": script})), text("Merged.")],
        100_000,
    )
    .await;
    let r = tool_results(&run(&h, "merge").await);
    assert!(!r[0].1 && r[0].2.contains("Merge branch 'side'"), "{r:?}");
}

// ---- apply_patch ---------------------------------------------------------------------

/// [`harness`] with a GPT-family model id.
async fn gpt_harness(script: Vec<Reply>) -> Harness {
    let mock = Mock::start(script).await;
    let dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = format!(
        r#"default_model = "mock/gpt-5.2-codex"
[agent]
review_interval = 0
[[provider]]
id = "mock"
name = "Mock"
kind = "openai_chat"
base_url = "{}"
discover = false
parallel = 4
[[provider.models]]
id = "gpt-5.2-codex"
context_window = 100000
max_output = 2000
[tuning."*"]
check = false
small = "off"
"#,
        mock.url
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();
    Harness {
        engine,
        mock,
        dir,
        work,
        _work_dir: work_dir,
    }
}

const PATCH: &str = "*** Begin Patch
*** Update File: calc.rb
@@ def add(a, b)
-  a - b
+  a + b
*** Add File: README.txt
+calc: add numbers
*** End Patch";

#[tokio::test]
async fn gpt_models_patches_are_applied_as_edits() {
    let h = gpt_harness(vec![
        tool("apply_patch", json!({"input": PATCH})),
        tool(
            "bash",
            json!({"command": format!("apply_patch <<'EOF'\n{}\nEOF", PATCH.replace("a + b", "a + b # fixed").replace("-  a - b", "-  a + b").replace("*** Add File: README.txt\n+calc: add numbers\n", ""))}),
        ),
        text("Fixed add."),
        text("No Ruby here."),
    ])
    .await;
    std::fs::write(h.work.join("calc.rb"), "def add(a, b)\n  a - b\nend\n").unwrap();
    let entries = run(&h, "fix add").await;
    let r = tool_results(&entries);
    assert_eq!(r[0].0, "edit", "{r:?}");
    assert!(!r[0].1, "{r:?}");
    assert!(
        r[0].2.contains("[calc.rb#") && r[0].2.contains("[README.txt#"),
        "{}",
        r[0].2
    );
    assert!(!r[1].1 && r[1].0 == "edit", "the shell form too: {r:?}");
    assert_eq!(
        std::fs::read_to_string(h.work.join("calc.rb")).unwrap(),
        "def add(a, b)\n  a + b # fixed\nend\n"
    );
    assert_eq!(
        std::fs::read_to_string(h.work.join("README.txt")).unwrap(),
        "calc: add numbers\n"
    );
}

#[tokio::test]
async fn other_models_are_not_offered_patches() {
    let h = harness(
        vec![tool("apply_patch", json!({"input": PATCH})), text("Hm.")],
        100_000,
    )
    .await;
    std::fs::write(h.work.join("calc.rb"), "def add(a, b)\n  a - b\nend\n").unwrap();
    let entries = run(&h, "fix add").await;
    let r = tool_results(&entries);
    assert!(
        r[0].1 && r[0].2.starts_with("Unknown tool `apply_patch`"),
        "{r:?}"
    );
    // Nothing about patches in what any model is sent.
    let req = h.mock.agent_requests()[0].to_string();
    assert!(!req.contains("Begin Patch") && !req.contains("apply_patch"));
}
