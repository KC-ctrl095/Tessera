//! Questions an agent asks with a card (a whiteboard's `ask`, Claude Code's
//! questions): answered here, in the chat, in place of the composer. What
//! else a whiteboard shows (pages, images, notes) is the apps' to show: the
//! chat says so in one line.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::Line;
use serde_json::Value;
use tessera_proto::*;

use super::app::App;
use super::pane::Pane;
use super::text::{self, span};
use super::theme::Theme;

/// The question waiting in this chat (or one of its subagents), unless put
/// aside.
pub fn pending_ask(
    p: &Pane,
    boards: &std::collections::HashMap<String, Whiteboard>,
) -> Option<(Whiteboard, BoardAsk)> {
    let sid = p.session_id()?;
    boards
        .values()
        .filter(|b| b.session_id == sid || p.children.iter().any(|c| c.id == b.session_id))
        .filter_map(|b| b.ask.as_ref().map(|a| (b, a)))
        .filter(|(_, a)| p.ask_hidden.as_deref() != Some(a.id.as_str()))
        .max_by_key(|(b, _)| b.updated_at)
        .map(|(b, a)| (b.clone(), a.clone()))
}

/// A question, as lines: the prompt, then what can be answered.
pub fn ask_lines(a: &BoardAsk, sel: usize, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out = text::wrap_plain(&text::sanitize(a.prompt.trim()), t.bold(), width, &[], &[]);
    out.push(Line::default());
    let keys = |items: &[(&str, &str)]| {
        let mut v = Vec::new();
        for (i, (k, d)) in items.iter().enumerate() {
            if i > 0 {
                v.push(span("   ", t.faint()));
            }
            v.push(span(k.to_string(), t.bold()));
            v.push(span(format!(" {d}"), t.muted()));
        }
        Line::from(v)
    };
    match &a.input {
        AskInput::Choice { options, .. } => {
            for (i, o) in options.iter().enumerate() {
                let on = i == sel.min(options.len().saturating_sub(1));
                let l = Line::from(vec![
                    span(if on { " › " } else { "   " }, t.accent()),
                    span(format!("{} ", i + 1), t.faint()),
                    span(
                        text::sanitize(o).replace('\n', " "),
                        if on {
                            t.accent_text().add_modifier(Modifier::BOLD)
                        } else {
                            t.text()
                        },
                    ),
                ]);
                let l = text::pad_line(l, width, text::Align::Left);
                out.push(if on { l.patch_style(t.selected()) } else { l });
            }
            out.push(Line::default());
            out.push(keys(&[("↑↓", "choose"), ("⏎", "answer"), ("esc", "later")]));
        }
        AskInput::Confirm => out.push(keys(&[("y", "yes"), ("n", "no"), ("esc", "later")])),
        AskInput::Text { placeholder, .. } => {
            if !placeholder.trim().is_empty() {
                out.push(Line::from(span(
                    text::sanitize(placeholder.trim()).into_owned(),
                    t.faint(),
                )));
            }
            out.push(keys(&[
                ("⏎", "answer"),
                ("ctrl+x", "decline"),
                ("esc", "later"),
            ]));
        }
        AskInput::Signature => {
            out.push(Line::from(span(
                "Sign in the Tessera app on your phone or computer.",
                t.muted(),
            )));
            out.push(keys(&[("ctrl+x", "decline"), ("esc", "later")]));
        }
    }
    out
}

/// One plain line for what a whiteboard shows (the apps draw it).
fn shown_line(b: &Whiteboard) -> String {
    let what = match &b.content {
        BoardContent::Html { .. } => "a page",
        BoardContent::Markdown { .. } => "notes",
        BoardContent::Image { .. } => "an image",
        BoardContent::Url { .. } => "a web page",
        BoardContent::Local { .. } => "a web app",
    };
    format!(
        "The agent shows {what}, “{}”: open the chat in the Tessera app to see it.",
        text::sanitize(&b.title)
    )
}

impl App {
    /// The boards of a chat (for questions that were already waiting).
    pub(super) fn fetch_session_boards(&mut self, session_id: &str) {
        self.call::<Vec<Whiteboard>, _>(
            Request::ListBoards {
                session_id: Some(session_id.to_string()),
            },
            |app, r| {
                if let Ok(list) = r {
                    for b in list {
                        app.keep_board(b);
                    }
                }
            },
        );
    }

    /// A new or changed board: its question is answered here; anything new
    /// it shows gets one line in its chat.
    pub(super) fn board_arrived(&mut self, b: Whiteboard) {
        let new = !self.boards.contains_key(&b.id);
        if new && !b.by_user && b.ask.is_none() {
            for p in self
                .panes
                .iter_mut()
                .filter(|p| p.session_id() == Some(b.session_id.as_str()))
            {
                p.transcript.add_local(NoticeLevel::Info, shown_line(&b));
            }
        }
        self.keep_board(b);
    }

    /// Keep the newest version of a board; a new question resets the
    /// choice.
    fn keep_board(&mut self, b: Whiteboard) {
        let Some(old) = self.boards.get(&b.id) else {
            self.boards.insert(b.id.clone(), b);
            self.dirty = true;
            return;
        };
        if old.version > b.version {
            return;
        }
        let asked = b.ask.as_ref().map(|a| a.id.clone());
        let had = old.ask.as_ref().map(|a| a.id.clone());
        if asked != had {
            for p in self.panes.iter_mut() {
                p.ask_sel = 0;
            }
        }
        self.boards.insert(b.id.clone(), b);
        self.dirty = true;
    }

    /// Answer a board's question.
    pub(super) fn answer_board(&mut self, board_id: String, ask_id: String, answer: AskAnswer) {
        self.toast(match &answer {
            AskAnswer::Cancel => "Declined",
            _ => "Answered",
        });
        let pid = self.focus;
        self.call::<Value, _>(
            Request::AnswerBoard {
                id: board_id,
                ask_id,
                answer,
            },
            move |app, r| {
                if let Err(e) = r {
                    app.toast_warn(format!("The answer didn't arrive: {}", e.message));
                    if let Some(p) = app.pane_by_mut(pid) {
                        p.ask_hidden = None;
                    }
                }
            },
        );
    }

    /// Keys for a question. Returns true when used.
    pub(super) fn ask_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        let Some((b, a)) = pending_ask(self.pane(), &self.boards) else {
            return false;
        };
        match (&a.input, k.code) {
            (_, KeyCode::Char('x')) if ctrl => {
                self.answer_board(b.id, a.id, AskAnswer::Cancel);
                true
            }
            (_, KeyCode::Esc) => {
                self.pane_mut().ask_hidden = Some(a.id.clone());
                self.toast("Put aside: the apps still show the question");
                true
            }
            (AskInput::Choice { options, .. }, code) => {
                let n = options.len();
                if n == 0 {
                    return false;
                }
                match code {
                    KeyCode::Up => {
                        let p = self.pane_mut();
                        p.ask_sel = p.ask_sel.checked_sub(1).unwrap_or(n - 1);
                        true
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        let p = self.pane_mut();
                        p.ask_sel = (p.ask_sel + 1) % n;
                        true
                    }
                    KeyCode::Enter => {
                        let i = self.pane().ask_sel.min(n - 1);
                        self.answer_board(b.id, a.id, AskAnswer::Choice { index: i as u32 });
                        true
                    }
                    KeyCode::Char(c @ '1'..='9') if !ctrl => {
                        let i = c as usize - '1' as usize;
                        if i < n {
                            self.answer_board(b.id, a.id, AskAnswer::Choice { index: i as u32 });
                        }
                        true
                    }
                    _ => true,
                }
            }
            (AskInput::Confirm, KeyCode::Char('y' | 'Y')) if !ctrl => {
                self.answer_board(b.id, a.id, AskAnswer::Confirm { yes: true });
                true
            }
            (AskInput::Confirm, KeyCode::Char('n' | 'N')) if !ctrl => {
                self.answer_board(b.id, a.id, AskAnswer::Confirm { yes: false });
                true
            }
            (AskInput::Confirm, _) => true,
            (AskInput::Text { multiline, .. }, KeyCode::Enter) => {
                let shift = k.modifiers.contains(KeyModifiers::SHIFT);
                if *multiline && (shift || ctrl) {
                    self.pane_mut().editor.insert_char('\n');
                    return true;
                }
                let text_s = self.pane().editor.text().trim().to_string();
                if text_s.is_empty() {
                    self.toast_warn("Type an answer first (ctrl+x declines)");
                    return true;
                }
                self.pane_mut().editor.clear();
                self.answer_board(b.id, a.id, AskAnswer::Text { text: text_s });
                true
            }
            (AskInput::Signature, _) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::tests::{key, test_app};
    use crate::tui::pane::tests::info;

    fn board(id: &str, session: &str, ask: Option<BoardAsk>) -> Whiteboard {
        Whiteboard {
            id: id.into(),
            session_id: session.into(),
            title: "Pick one".into(),
            content: BoardContent::Markdown {
                markdown: String::new(),
            },
            version: 1,
            created_at: 0,
            updated_at: 1,
            ask,
            by_user: false,
            host: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn questions_are_answered_in_the_chat_and_boards_get_one_line() {
        let (mut app, dir) = test_app().await;
        app.pane_mut()
            .apply_info(info("s1", SessionStatus::Running));
        // A board without a question: one line in its chat.
        app.board_arrived(board("b0", "s1", None));
        let lines = app.pane().transcript.user_texts();
        assert!(lines.is_empty());
        assert!(!app.pane().transcript.is_empty(), "a notice was added");
        // A choice: ↓ moves, typing doesn't reach the composer.
        let ask = BoardAsk {
            id: "a1".into(),
            prompt: "Which one?".into(),
            input: AskInput::Choice {
                options: vec!["Red".into(), "Blue".into()],
                multi: false,
            },
        };
        app.board_arrived(board("b1", "s1", Some(ask.clone())));
        assert!(pending_ask(app.pane(), &app.boards).is_some());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.pane().ask_sel, 1);
        app.on_key(key(KeyCode::Char('z')));
        assert!(app.pane().editor.is_empty());
        let t = Theme::test();
        let text: Vec<String> = ask_lines(&ask, 1, 40, &t)
            .iter()
            .map(text::line_text)
            .collect();
        assert_eq!(text[0], "Which one?");
        assert!(text.iter().any(|l| l.starts_with(" › 2 Blue")), "{text:?}");
        // Esc puts it aside.
        app.on_key(key(KeyCode::Esc));
        assert!(pending_ask(app.pane(), &app.boards).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
