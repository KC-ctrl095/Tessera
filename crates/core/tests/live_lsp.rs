//! Live check of diagnostics after edits and the `lsp` tool against a local
//! model and a real language server (ignored by default). Needs `clangd` and
//! a C compiler on PATH:
//!
//!   TESSERA_LIVE_MODEL=local/qwen3.8-27b cargo test -p tessera-core --test live_lsp -- --ignored --nocapture

use std::time::{Duration, Instant};

use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

const SOURCE: &str = "#include <stdio.h>\n\nint add(int a, int b) { return a + b; }\n\nint main(void) {\n    printf(\"%d\\n\", add(2, 3));\n    return 0;\n}\n";

const TASK: &str = "\
Work in calc.c, in this order, one edit per step:
1. In main, add the line `print_twice(7);` just before `return 0;` (the function doesn't exist yet; that's expected).
2. Then define `void print_twice(int n)` above main; it prints n twice on one line, e.g. \"7 7\".
3. Rename the function `add` to `sum` everywhere with the `lsp` tool's `rename` action.
4. Build and run: `cc -Wall -Werror -o calc calc.c && ./calc`.
Report the program's output.";

fn installed(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

#[tokio::test]
#[ignore]
async fn a_local_model_sees_new_errors_after_edits_and_renames_with_lsp() {
    let Ok(model) = std::env::var("TESSERA_LIVE_MODEL") else {
        eprintln!("set TESSERA_LIVE_MODEL (e.g. local/qwen3.8-27b)");
        return;
    };
    if !installed("clangd") || !installed("cc") {
        eprintln!("needs clangd and cc on PATH");
        return;
    }
    let url =
        std::env::var("TESSERA_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into());
    let (provider, id) = model.split_once('/').expect("provider/model");
    let home = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().join("calc");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("compile_flags.txt"), "-std=c11\n-Wall\n").unwrap();
    std::fs::write(work.join("calc.c"), SOURCE).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "default_model = \"{model}\"\n[agent]\nreview_interval = 0\npermission = \"auto\"\n[tuning.\"*\"]\nsmall = \"off\"\ncheck = false\n[[provider]]\nid = \"{provider}\"\nkind = \"openai_chat\"\nbase_url = \"{url}\"\ndiscover = false\n[[provider.models]]\nid = \"{id}\"\ncontext_window = 131072\nreasoning = true\n"
        ),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(home.path()), EngineOptions::default())
        .await
        .unwrap();
    let s: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: Some(work.display().to_string()),
                    model: None,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let started = Instant::now();
    engine
        .handle(
            Request::Send {
                session_id: s.id.clone(),
                content: vec![ContentPart::text(TASK)],
                delivery: Delivery::Auto,
                attachments: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(900), engine.wait_idle(&s.id))
        .await
        .expect("the task took too long");
    let snap: SessionSnapshot = serde_json::from_value(
        engine
            .handle(
                Request::GetSession {
                    session_id: s.id.clone(),
                    since_seq: None,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let mut results = Vec::new();
    for e in &snap.entries {
        match &e.body {
            EntryBody::ToolResult {
                tool,
                content,
                is_error,
                ..
            } => {
                let text: String = content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!(
                    "--- {tool}{}\n{text}",
                    if *is_error { " (error)" } else { "" }
                );
                results.push((tool.clone(), text));
            }
            EntryBody::Assistant { content, .. } => {
                for p in content {
                    match p {
                        AssistantPart::Text { text } => eprintln!(">>> {text}"),
                        AssistantPart::ToolCall {
                            name, arguments, ..
                        } => {
                            eprintln!(">>> call {name} {arguments}")
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    eprintln!(
        "{} steps, {:.0}s, {} input / {} output tokens",
        snap.info.steps,
        started.elapsed().as_secs_f32(),
        snap.info.usage.input,
        snap.info.usage.output
    );
    let code = std::fs::read_to_string(work.join("calc.c")).unwrap();
    eprintln!("=== calc.c\n{code}");
    // The first edit's result told the model about the missing function.
    assert!(
        results.iter().any(|(t, r)| (t == "edit" || t == "write")
            && r.contains("LSP clangd:")
            && r.contains("print_twice")),
        "no edit result carried the clangd error"
    );
    assert!(
        results
            .iter()
            .any(|(t, r)| t == "lsp" && r.starts_with("Renamed `add` to `sum`"))
    );
    assert!(code.contains("sum(") && !code.contains("add("), "{code}");
    let run = std::process::Command::new("sh")
        .arg("-c")
        .arg("cc -Wall -Werror -o calc calc.c && ./calc")
        .current_dir(&work)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && out.contains('5') && out.contains("7 7"),
        "{out}"
    );
}
