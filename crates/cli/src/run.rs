//! `tessera run`: non-interactive mode for scripts, pipes, benchmarks and
//! quick answers. The answer streams to stdout; activity goes to stderr.
//!
//! Exit codes: 0 finished, 1 error, 2 stopped by a step limit (`--max-steps`
//! or `agent.max_steps`), 3 an action needed approval that a
//! non-interactive run can't give (re-run with `--yes`). With `--json` the
//! protocol events stream as JSON lines and the last line is the result:
//! `{"type":"result","status":…,"session_id":…,"steps":…,"usage":…,"text":…}`.

use std::io::{IsTerminal, Read, Write};

use anyhow::{Result, anyhow, bail};
use tessera_proto::*;

use crate::backend::{Backend, BackendEvent};
// Escaping every control immediately is chunk-invariant: even a lone ESC
// is visible before the next chunk arrives. No buffered ANSI sequence can
// consume later text, hide an approval, or reach the terminal.
use tessera_core::util::visible_controls_lines as terminal_text;

pub struct RunOptions {
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub session: Option<String>,
    pub continue_last: bool,
    pub thinking: Option<ThinkingLevel>,
    pub permission: Option<PermissionMode>,
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    /// Stop (exit 2) once the run has taken this many steps.
    pub max_steps: Option<u32>,
    /// A new chat gets every tool (as in the apps), not the terminal's lean
    /// coding set.
    pub all_tools: bool,
}

/// Exit codes (see the module docs).
pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_STEPS: i32 = 2;
pub const EXIT_APPROVAL: i32 = 3;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Finished,
    Error,
    StepLimit,
    NeedsApproval,
}

impl Ending {
    fn code(self) -> i32 {
        match self {
            Ending::Finished => EXIT_OK,
            Ending::Error => EXIT_ERROR,
            Ending::StepLimit => EXIT_STEPS,
            Ending::NeedsApproval => EXIT_APPROVAL,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Ending::Finished => "finished",
            Ending::Error => "error",
            Ending::StepLimit => "step_limit",
            Ending::NeedsApproval => "needs_approval",
        }
    }
}

/// The run's ending from what happened (worst first).
fn ending(failed: bool, step_limit: bool, denied: bool) -> Ending {
    if step_limit {
        Ending::StepLimit
    } else if failed {
        Ending::Error
    } else if denied {
        Ending::NeedsApproval
    } else {
        Ending::Finished
    }
}

/// The final `--json` line.
fn result_json(
    end: Ending,
    session_id: &str,
    steps: u32,
    usage: &Usage,
    text: &str,
    error: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "type": "result",
        "status": end.name(),
        "exit_code": end.code(),
        "session_id": session_id,
        "steps": steps,
        "usage": {
            "input": usage.input,
            "output": usage.output,
            "cache_read": usage.cache_read,
            "cache_write": usage.cache_write,
        },
        "text": text,
        "error": error,
    })
}

pub async fn run(backend: Backend, opts: RunOptions) -> Result<i32> {
    let mut prompt = opts.prompt.clone().unwrap_or_default();
    if !std::io::stdin().is_terminal() {
        let piped = read_stdin(prompt.is_empty()).await;
        if !piped.trim().is_empty() {
            prompt = if prompt.is_empty() {
                piped
            } else {
                format!("{prompt}\n\n<stdin>\n{piped}\n</stdin>")
            };
        }
    }
    if prompt.trim().is_empty() {
        bail!("nothing to do: pass a prompt or pipe input on stdin");
    }

    let cwd = opts.cwd.clone().or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string())
    });
    let session_id = match (&opts.session, opts.continue_last) {
        (Some(id), _) => id.clone(),
        (None, true) => {
            let list: Vec<SessionInfo> = backend
                .call_as(Request::ListSessions { archived: false })
                .await?;
            let cwd_s = cwd.clone().unwrap_or_default();
            list.iter()
                .find(|s| s.cwd == cwd_s)
                .or(list.first())
                .map(|s| s.id.clone())
                .ok_or_else(|| anyhow!("no previous session to continue"))?
        }
        (None, false) => {
            let model = opts
                .model
                .as_deref()
                .map(str::parse::<ModelRef>)
                .transpose()
                .map_err(|e| anyhow!(e))?;
            let info: SessionInfo = backend
                .call_as(Request::CreateSession {
                    cwd: cwd.clone(),
                    model,
                    title: None,
                    bot_id: None,
                    private: false,
                    lean: !opts.all_tools,
                })
                .await?;
            info.id
        }
    };
    if opts.thinking.is_some()
        || opts.permission.is_some()
        || (opts.model.is_some() && opts.session.is_some())
    {
        let model = opts
            .model
            .as_deref()
            .map(str::parse::<ModelRef>)
            .transpose()
            .map_err(|e| anyhow!(e))?;
        backend
            .call(Request::UpdateSession {
                session_id: session_id.clone(),
                title: None,
                model,
                thinking: opts.thinking,
                permission: opts.permission,
                pinned: None,
                archived: None,
                cwd: None,
                members: None,
                fast: None,
                subagents: None,
            })
            .await?;
    }

    // Where the session stands before this run (steps and tokens are
    // counted from here).
    let before: Option<SessionInfo> = backend
        .call_as::<SessionSnapshot>(Request::GetSession {
            session_id: session_id.clone(),
            since_seq: Some(u64::MAX),
        })
        .await
        .ok()
        .map(|s| s.info);
    let base_steps = before.as_ref().map(|i| i.steps).unwrap_or(0);
    let base_usage = before.map(|i| i.usage).unwrap_or_default();
    let mut steps_now = base_steps;
    let mut usage_now = base_usage.clone();

    let mut events = backend.subscribe();
    backend
        .call(Request::Send {
            session_id: session_id.clone(),
            content: vec![ContentPart::text(prompt)],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        })
        .await?;

    let mut out = std::io::stdout();
    let err_tty = std::io::stderr().is_terminal();
    // Model and tool text is sanitized: escape sequences in it could
    // otherwise rewrite what the terminal shows (an approval prompt, say).
    let dim = |s: &str| {
        let s = terminal_text(s);
        if err_tty {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s
        }
    };
    let mut seen_running = false;
    let mut final_text = String::new();
    let mut current_text = String::new();
    let mut streamed_bytes = 0;
    let mut failed: Option<String> = None;
    let mut step_limit = false;
    let mut denied = false;
    // Only machines that ask before every action were denied: --yes won't help.
    let mut only_machines = true;
    let mut ends_with_newline = true;

    while let Some(ev) = events.recv().await {
        let BackendEvent::Event(mut ev) = ev else {
            continue;
        };
        if ev.session_id() != Some(session_id.as_str()) {
            continue;
        }
        // Old servers may still send protocol entries and text deltas. Hold
        // an ambiguous text prefix until it becomes a real answer, including
        // in JSON output (not just the final-answer selector).
        if matches!(&ev, Event::EntryAdded { entry, .. } if tessera_proto::is_protocol_entry(entry))
        {
            continue;
        }
        match &mut ev {
            Event::StreamStarted { .. } => {
                current_text.clear();
                streamed_bytes = 0;
            }
            Event::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } => {
                if !release_text(&mut current_text, text, &mut streamed_bytes) {
                    continue;
                }
            }
            Event::StreamEnded { .. }
                if streamed_bytes == 0
                    && !current_text.is_empty()
                    && !tessera_proto::is_check_ack(&current_text) =>
            {
                if opts.json {
                    println!(
                        "{}",
                        serde_json::to_string(&Event::StreamDelta {
                            session_id: session_id.clone(),
                            delta: StreamDelta::Text {
                                index: 0,
                                text: current_text.clone()
                            }
                        })?
                    );
                } else if !opts.quiet {
                    out.write_all(terminal_text(&current_text).as_bytes())?;
                    out.flush()?;
                    ends_with_newline = current_text.ends_with('\n');
                }
                streamed_bytes = current_text.len();
            }
            _ => {}
        }
        if opts.json {
            println!("{}", serde_json::to_string(&ev)?);
        }
        match ev {
            Event::StreamStarted { .. } => {}
            Event::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } => {
                if !opts.json && !opts.quiet {
                    out.write_all(terminal_text(&text).as_bytes())?;
                    out.flush()?;
                    ends_with_newline = text.ends_with('\n');
                }
            }
            Event::StreamDelta {
                delta: StreamDelta::Thinking { text, .. },
                ..
            } if opts.verbose && !opts.json => {
                eprint!("{}", dim(&text));
            }
            Event::ToolOutput { chunk, .. } if opts.verbose && !opts.json => {
                eprint!("{}", dim(&chunk))
            }
            Event::EntryAdded { entry, .. } => match entry.body {
                EntryBody::Assistant { content, .. } => {
                    let text: String = content
                        .iter()
                        .filter_map(|p| {
                            if let AssistantPart::Text { text } = p {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !text.trim().is_empty() {
                        final_text = text;
                    }
                    if !opts.json {
                        if !ends_with_newline
                            && !opts.quiet
                            && content
                                .iter()
                                .any(|p| matches!(p, AssistantPart::ToolCall { .. }))
                        {
                            println!();
                            ends_with_newline = true;
                        }
                        for p in &content {
                            if let AssistantPart::ToolCall {
                                name, arguments, ..
                            } = p
                            {
                                let summary = arguments
                                    .as_object()
                                    .and_then(|o| o.values().find_map(|v| v.as_str()))
                                    .map(|s| tessera_core::util::one_line(s, 100))
                                    .unwrap_or_default();
                                if !opts.quiet || opts.verbose {
                                    eprintln!("{}", dim(&format!("● {name} {summary}")));
                                }
                            }
                        }
                    }
                }
                EntryBody::ToolResult {
                    tool,
                    is_error: true,
                    content,
                    ..
                } if !opts.json => {
                    eprintln!(
                        "{}",
                        dim(&format!(
                            "  ✗ {tool}: {}",
                            tessera_core::util::one_line(&plain_text(&content), 160)
                        ))
                    );
                }
                EntryBody::Notice { level, text, .. } if !opts.json => {
                    if level != NoticeLevel::Info || opts.verbose {
                        eprintln!("{}", dim(&format!("[{level:?}] {text}").to_lowercase()));
                    }
                    if level == NoticeLevel::Error {
                        if text.contains("agent.max_steps") {
                            step_limit = true;
                        }
                        failed = Some(text);
                    }
                }
                EntryBody::Notice {
                    level: NoticeLevel::Error,
                    text,
                    ..
                } => {
                    if text.contains("agent.max_steps") {
                        step_limit = true;
                    }
                    failed = Some(text);
                }
                EntryBody::Compaction {
                    tokens_before,
                    tokens_after,
                    ..
                } if !opts.json => {
                    eprintln!(
                        "{}",
                        dim(&format!(
                            "[context compacted {} → {} tokens]",
                            crate::tui::text::tokens(tokens_before),
                            crate::tui::text::tokens(tokens_after)
                        ))
                    );
                }
                _ => {}
            },
            Event::ApprovalRequested { request, .. } => {
                let decision = if std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
                {
                    ask(&request)?
                } else {
                    if !opts.json {
                        // A machine that asks before every action asks even
                        // with --yes: only a person can approve it.
                        let hint = if request.tool == "machines" {
                            "Approve it in an app or the terminal UI, where you can answer"
                        } else {
                            "Re-run with --yes to allow"
                        };
                        eprintln!(
                            "{}",
                            dim(&format!(
                                "✗ denied (non-interactive): {} — {}. {hint}.",
                                request.summary,
                                request.reason.trim_end_matches('.')
                            ))
                        );
                    }
                    denied = true;
                    only_machines &= request.tool == "machines";
                    ApprovalDecision::Deny
                };
                backend
                    .call(Request::RespondApproval {
                        session_id: session_id.clone(),
                        request_id: request.id,
                        decision,
                    })
                    .await?;
            }
            Event::SessionUpdated { session } => {
                steps_now = session.steps.max(steps_now);
                usage_now = session.usage.clone();
                if let Some(max) = opts.max_steps
                    && !step_limit
                    && steps_now.saturating_sub(base_steps) >= max
                    && matches!(
                        session.status,
                        SessionStatus::Running | SessionStatus::AwaitingApproval
                    )
                {
                    step_limit = true;
                    if !opts.json {
                        eprintln!("{}", dim(&format!("[stopping: --max-steps {max} reached]")));
                    }
                    let _ = backend
                        .call(Request::Abort {
                            session_id: session_id.clone(),
                        })
                        .await;
                }
                match session.status {
                    SessionStatus::Running
                    | SessionStatus::AwaitingApproval
                    | SessionStatus::Compacting => seen_running = true,
                    SessionStatus::Idle | SessionStatus::Interrupted if seen_running => break,
                    SessionStatus::Error if seen_running => {
                        failed.get_or_insert_with(|| "the run failed".into());
                        break;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if opts.quiet && !opts.json {
        println!("{}", terminal_text(final_text.trim_end()));
    } else if !ends_with_newline && !opts.json {
        println!();
    }
    let end = ending(failed.is_some(), step_limit, denied);
    if opts.json {
        let usage = Usage {
            input: usage_now.input.saturating_sub(base_usage.input),
            output: usage_now.output.saturating_sub(base_usage.output),
            cache_read: usage_now.cache_read.saturating_sub(base_usage.cache_read),
            cache_write: usage_now.cache_write.saturating_sub(base_usage.cache_write),
            ..usage_now.clone()
        };
        println!(
            "{}",
            result_json(
                end,
                &session_id,
                steps_now.saturating_sub(base_steps),
                &usage,
                final_text.trim(),
                failed.as_deref(),
            )
        );
    } else {
        match end {
            Ending::StepLimit => eprintln!(
                "stopped: step limit reached{}",
                opts.max_steps
                    .map(|m| format!(" (--max-steps {m})"))
                    .unwrap_or_default()
            ),
            Ending::Error => {
                if let Some(e) = &failed {
                    eprintln!("error: {}", terminal_text(e));
                }
            }
            Ending::NeedsApproval if only_machines => eprintln!(
                "needs approval: a machine asks before every action, and nobody here could approve it; approve it in an app or the terminal UI"
            ),
            Ending::NeedsApproval => eprintln!(
                "needs approval: an action was denied because nobody could approve it; re-run with --yes to allow"
            ),
            Ending::Finished => {}
        }
    }
    Ok(end.code())
}

/// Read piped stdin. With a prompt argument, stdin is optional: an idle,
/// never-closed stdin (cron, CI, tool runners) must not hang the run.
async fn read_stdin(required: bool) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    if required {
        return rx.await.unwrap_or_default();
    }
    let mut rx = rx;
    match tokio::time::timeout(std::time::Duration::from_millis(250), &mut rx).await {
        Ok(r) => r.unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// The old-server fallback for streamed text, shared by terminal and JSON
/// output. True means `text` is a visible delta (including any held prefix).
fn release_text(buffer: &mut String, text: &mut String, emitted: &mut usize) -> bool {
    buffer.push_str(text);
    if *emitted == 0 {
        let prefix: String = buffer.chars().take(32).collect();
        if tessera_core::tools::meta::possible_check_ack(&prefix) {
            return false;
        }
        text.clone_from(buffer);
    }
    *emitted = buffer.len();
    true
}
/// What the user is asked to approve, with nothing in it that the terminal
/// would interpret (the summary is model-written).
fn approval_text(req: &ApprovalRequest) -> String {
    format!(
        "\n\x1b[1;33m? Approval needed\x1b[0m {} — {}\n  {}",
        terminal_text(&req.tool),
        terminal_text(&req.reason),
        terminal_text(&req.summary)
    )
}

fn ask(req: &ApprovalRequest) -> Result<ApprovalDecision> {
    eprintln!("{}", approval_text(req));
    if req.once {
        // Asked every time (the machine's own rule): nothing to remember.
        eprint!("  Allow? [y]es / [N]o: ");
    } else {
        eprint!("  Allow? [y]es / [a]lways this session / [N]o: ");
    }
    std::io::stderr().flush()?;
    let mut line = String::new();
    // stdin is the terminal here (the prompt came from args).
    std::io::stdin().read_line(&mut line)?;
    Ok(match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalDecision::Allow,
        "a" | "always" => ApprovalDecision::AllowSession,
        _ => ApprovalDecision::Deny,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_text_is_safe_and_identical_at_every_stream_split() {
        for input in [
            "hello 你好\n\tworld",
            "\x1b[2K\rprevious\x08next",
            "\x1b]52;c;Zml4dHVyZQ==\x07answer",
            "\x1b]0;title\x1b\\answer",
            "\u{9b}31mred\u{9d}52;c;value\u{9c}",
            "a\u{202e}txt.exe\u{200b}\0end\x1b",
        ] {
            let expected = terminal_text(input);
            assert!(
                !expected
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            );
            for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
                assert_eq!(
                    format!(
                        "{}{}",
                        terminal_text(&input[..split]),
                        terminal_text(&input[split..])
                    ),
                    expected
                );
            }
            assert_eq!(
                input
                    .chars()
                    .map(|c| terminal_text(&c.to_string()))
                    .collect::<String>(),
                expected
            );
        }
        assert_eq!(terminal_text("a\x1b[2K\r\n\tb"), "a\\u{1b}[2K\\r\n\tb");
        assert_eq!(terminal_text("你好\n\t🙂"), "你好\n\t🙂");
    }

    #[test]
    fn json_output_keeps_the_original_text_as_escaped_json() {
        let text = "\x1b]52;c;fixture\x07";
        let value = result_json(Ending::Finished, "s", 0, &Usage::default(), text, None);
        assert_eq!(value["text"], text);
        let encoded = value.to_string();
        assert!(!encoded.contains('\x1b') && !encoded.contains('\x07'));
    }

    #[test]
    fn legacy_streamed_marks_are_held_but_checked_answers_are_released_once() {
        let mut buffer = String::new();
        let mut emitted = 0;
        for piece in ["D", "one ", "✓"] {
            let mut text = piece.to_owned();
            assert!(!release_text(&mut buffer, &mut text, &mut emitted));
        }
        assert!(tessera_proto::is_check_ack(&buffer));
        assert_eq!(emitted, 0);
        buffer.clear();
        for piece in ["✓", " "] {
            assert!(!release_text(
                &mut buffer,
                &mut piece.to_owned(),
                &mut emitted
            ));
        }
        let mut text = "Tests pass".to_owned();
        assert!(release_text(&mut buffer, &mut text, &mut emitted));
        assert_eq!(text, "✓ Tests pass");
        let mut text = ". More details.".to_owned();
        assert!(release_text(&mut buffer, &mut text, &mut emitted));
        assert_eq!(text, ". More details.");
        assert_eq!(buffer, "✓ Tests pass. More details.");
        assert_eq!(emitted, buffer.len());
    }
    #[test]
    fn endings_and_exit_codes() {
        assert_eq!(ending(false, false, false).code(), 0);
        assert_eq!(ending(true, false, false).code(), 1);
        assert_eq!(
            ending(true, true, false).code(),
            2,
            "a step limit is not an error"
        );
        assert_eq!(ending(false, false, true).code(), 3);
        assert_eq!(ending(true, false, true).code(), 1);
        let usage = Usage {
            input: 10,
            output: 2,
            ..Usage::default()
        };
        let v = result_json(Ending::StepLimit, "s1", 7, &usage, "done", None);
        assert_eq!(v["type"], "result");
        assert_eq!(v["status"], "step_limit");
        assert_eq!(v["exit_code"], 2);
        assert_eq!(v["steps"], 7);
        assert_eq!(v["usage"]["input"], 10);
        assert_eq!(v["text"], "done");
        assert!(v["error"].is_null());
    }

    #[test]
    fn approval_prompt_shows_the_real_command() {
        let req = ApprovalRequest {
            id: "a".into(),
            call_id: "c".into(),
            tool: "bash".into(),
            summary: "rm -rf ~/x\x1b[2K\r\x1b[1A  ls".into(),
            reason: "deletes files".into(),
            arguments: serde_json::json!({}),
            once: false,
        };
        let text = approval_text(&req);
        assert!(
            text.contains("rm -rf ~/x\\u{1b}[2K\\r\\u{1b}[1A  ls"),
            "{text:?}"
        );
        // Only our own styling escapes remain.
        assert_eq!(text.matches('\x1b').count(), 2, "{text:?}");
    }
}
