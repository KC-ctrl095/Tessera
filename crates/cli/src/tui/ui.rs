//! Drawing. Everything here reads `App` state; the only writes are layout
//! feedback (pane rects, input width, page size, scroll clamping, mouse
//! targets, render caches).
//!
//! The screen is the chat: the conversation, the plan and queued messages,
//! the message box (or an approval or a question in its place) and one
//! status line under it. Chats side by side (`/split`) are columns with a
//! hairline between them, each with its title on top. Popups (lists, the
//! help, the rewind preview) float in the middle.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear};
use tessera_client::ConnState;
use tessera_proto::*;

use super::app::{App, Boot, Hit, Modal, PickKind, Picker, perm_label, thinking_name};
use super::ask;
use super::markdown;
use super::motif::{self, Sq};
use super::pane::{ApprovalView, CompKind, Live, LiveCache, Pane};
use super::text::{self, span};
use super::theme::Theme;
use super::transcript::{self, BotLook, Env, Speaker};

/// Widest the conversation gets (a readable column, centred).
const COLUMN: u16 = 104;

/// Read-only state every pane needs, split from `App` so panes can be
/// borrowed mutably at the same time.
struct Ctx<'a> {
    theme: Theme,
    expand: bool,
    thinking: bool,
    plan_collapsed: bool,
    /// Working-indicator step.
    step: usize,
    now: Instant,
    boot: Boot,
    conn: ConnState,
    bots: &'a std::collections::HashMap<String, BotLook>,
    boards: &'a std::collections::HashMap<String, Whiteboard>,
    providers: &'a [ProviderInfo],
    modal: bool,
    kbd_enhanced: bool,
    /// Several chats side by side.
    several: bool,
    /// Side-by-side chats that don't fit the window now (it was narrowed):
    /// the focused chat's title says so.
    hidden: usize,
    /// What the status line of the focused chat says instead (a toast, the
    /// second ctrl+c).
    notice: Option<(String, Style)>,
}

impl Ctx<'_> {
    fn spinner(&self) -> Vec<Span<'static>> {
        motif::spinner(&self.theme, self.step, 3)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.screen = area;
    app.hits.clear();
    for p in app.panes.iter_mut() {
        p.rect = Rect::default();
        p.trans_rect = Rect::default();
        p.input_rect = Rect::default();
    }
    if area.width == 0 || area.height == 0 {
        return;
    }
    let t = app.theme;
    if t.surfaces() {
        f.buffer_mut().set_style(area, t.stone(false));
    }
    if area.width < 20 || area.height < 6 {
        let msg = Line::from(vec![
            motif::logo(&t),
            span(" the window is too small", t.dim()),
        ]);
        f.buffer_mut().set_line(area.x, area.y, &msg, area.width);
        return;
    }
    let notice = if app.ctrl_c_armed() {
        Some(("Press ctrl+c again to quit".to_string(), t.warning()))
    } else {
        app.toast.as_ref().map(|t| (t.text.clone(), t.style))
    };
    // As many chats as fit (a narrowed window), the focused one among them.
    let room = app.room_for().min(app.panes.len()).max(1);
    let focus_i = app
        .panes
        .iter()
        .position(|p| p.id == app.focus)
        .unwrap_or(0);
    let first = focus_i.min(app.panes.len().saturating_sub(room));
    let shown = first..first + room;
    let n = room as u16;
    let focus = app.focus;
    let ctx = Ctx {
        theme: t,
        expand: app.expand,
        thinking: app.show_thinking,
        plan_collapsed: app.plan_collapsed,
        step: app.anim_step(),
        now: Instant::now(),
        boot: app.boot,
        conn: app.conn,
        bots: &app.bot_looks,
        boards: &app.boards,
        providers: &app.providers,
        modal: app.modal.is_some(),
        kbd_enhanced: app.kbd_enhanced,
        several: app.panes.len() > 1,
        hidden: app.panes.len() - room,
        notice,
    };
    // Equal columns with a hairline between them.
    let gap = if n > 1 { 1 } else { 0 };
    let w = (area.width.saturating_sub(gap * (n - 1))) / n;
    let mut x = area.x;
    let mut hits = Vec::new();
    let mut completion = None;
    let last = shown.end - 1;
    for (i, p) in app.panes.iter_mut().enumerate() {
        if !shown.contains(&i) {
            continue;
        }
        let pw = if i == last {
            area.right().saturating_sub(x)
        } else {
            w
        };
        let r = Rect::new(x, area.y, pw, area.height);
        let focused = p.id == focus;
        hits.push((r, Hit::Pane(p.id)));
        draw_pane(f, r, &ctx, p, focused);
        if focused {
            completion = Some(i);
        }
        x += pw;
        if i < last {
            let buf = f.buffer_mut();
            for y in area.y..area.bottom() {
                buf.set_line(x, y, &Line::from(span("│", t.hairline())), 1);
            }
            x += gap;
        }
    }
    app.hits = hits;
    if app.modal.is_none() {
        if let Some(i) = completion {
            draw_completion(f, &t, &app.panes[i]);
        }
    } else {
        draw_modal(f, area, app);
    }
}

/// A readable column inside `r`: a cell of margin on each side, centred
/// and no wider than [`COLUMN`] on wide windows.
fn column(r: Rect) -> Rect {
    if r.width > COLUMN + 4 {
        return Rect::new(r.x + (r.width - COLUMN) / 2, r.y, COLUMN, r.height);
    }
    if r.width >= 24 {
        return Rect::new(r.x + 1, r.y, r.width - 2, r.height);
    }
    r
}

/// One chat: (title) · conversation · plan · queue · message box or card ·
/// status line.
fn draw_pane(f: &mut Frame, r: Rect, ctx: &Ctx<'_>, p: &mut Pane, focused: bool) {
    let r = r.intersection(f.area());
    if r.width < 12 || r.height < 4 {
        return;
    }
    p.rect = r;
    let t = ctx.theme;
    let mut body = r;
    if ctx.several {
        // The chat's title, so side-by-side chats tell themselves apart.
        let title = p
            .session
            .as_ref()
            .map(|s| text::sanitize(&s.title).into_owned())
            .unwrap_or_else(|| "New chat".into());
        let mut left = vec![span(" ", Style::new())];
        if p.busy() {
            left.extend(ctx.spinner());
            left.push(span(" ", Style::new()));
        } else {
            left.push(span(if focused { "● " } else { "○ " }, t.accent()));
        }
        left.push(span(title, if focused { t.bold() } else { t.muted() }));
        // Chats that don't fit beside it are a keypress away.
        let more = Line::from(if focused && ctx.hidden > 0 {
            vec![
                span(format!("{} more ", ctx.hidden), t.faint()),
                span("alt+←/→", t.key()),
            ]
        } else {
            Vec::new()
        });
        let more_w = text::line_width(&more);
        let room = (r.width as usize).saturating_sub(if more_w > 0 { more_w + 2 } else { 0 });
        let mut line = text::truncate_line(Line::from(left), room);
        if more_w > 0 {
            let pad = room.saturating_sub(text::line_width(&line)) + 1;
            line.spans.push(span(" ".repeat(pad), Style::new()));
            line.spans.extend(more.spans);
        }
        f.buffer_mut().set_line(r.x, r.y, &line, r.width);
        body = Rect::new(r.x, r.y + 1, r.width, r.height - 1);
    }
    let col = column(body);
    let h = body.height;
    // Bottom-up: status line, message box (or a card), queue, plan.
    let status_h: u16 = 1;
    let approval = p.approvals.first().cloned();
    let asked = ask::pending_ask(p, ctx.boards);
    let card_w = col.width.saturating_sub(6) as usize;
    let (mut card, card_title): (Vec<Line<'static>>, Option<(String, Style)>) =
        if let Some(req) = &approval {
            (
                approval_lines(req, card_w, &t),
                Some((
                    " Approval needed ".into(),
                    t.warning().add_modifier(Modifier::BOLD),
                )),
            )
        } else if let Some((b, a)) = asked.as_ref().filter(|(_, a)| !is_text_ask(a)) {
            (
                ask::ask_lines(a, p.ask_sel, card_w, &t),
                Some((
                    format!(
                        " {} ",
                        text::truncate(&text::sanitize(&b.title), card_w.saturating_sub(4))
                    ),
                    t.accent_text().add_modifier(Modifier::BOLD),
                )),
            )
        } else {
            (Vec::new(), None)
        };
    let iw = col.width.saturating_sub(6).max(1) as usize;
    p.input_width = iw;
    let lay = p.editor.layout(iw);
    let text_ask = asked.as_ref().filter(|(_, a)| is_text_ask(a)).cloned();
    // The answer keys stay under an approval's text, however long it is.
    let keys_h = if approval.is_some() { 2 } else { 0 };
    let want_input: u16 = if card_title.is_some() {
        (card.len() + keys_h) as u16 + 2
    } else {
        lay.rows.len().clamp(1, 8) as u16 + 2
    };
    let min_trans = if h >= 12 { 3 } else { 1 };
    let input_h = want_input.min(h.saturating_sub(min_trans + status_h));
    if let Some(req) = &approval {
        // Text longer than the room scrolls; what part is shown is kept.
        let rows = (input_h as usize).saturating_sub(2 + keys_h);
        let v = &mut p.approval_view;
        if v.id != req.id || v.width != card_w {
            *v = ApprovalView {
                id: req.id.clone(),
                width: card_w,
                ..Default::default()
            };
        }
        let total = card.len();
        v.total = total;
        v.rows = rows;
        v.scroll = v.scroll.min(total.saturating_sub(rows));
        v.below = total.saturating_sub(v.scroll + rows);
        // What is on screen now counts only where it carries on from what
        // was already shown (and nothing counts without room for text).
        if rows > 0 && v.scroll <= v.seen {
            v.seen = v.seen.max((v.scroll + rows).min(total));
        }
        v.read = total > 0 && v.seen >= total;
        card = card.into_iter().skip(v.scroll).take(rows).collect();
        card.push(Line::default());
        card.push(approval_keys(req, p.approvals.len(), v, card_w, &t));
    }
    let plan = plan_lines(p, ctx, col.width as usize);
    let mut queued = queue_lines(p, &t, col.width as usize, focused);
    if let Some((b, a)) = &text_ask {
        // A question answered in the message box.
        let mut q = vec![Line::from(vec![
            span("  ◆ ", t.accent()),
            span(text::sanitize(&b.title).into_owned(), t.accent_text()),
        ])];
        q.extend(
            ask::ask_lines(a, 0, col.width.saturating_sub(4) as usize, &t)
                .into_iter()
                .map(|mut l| {
                    l.spans.insert(0, span("    ", Style::new()));
                    l
                }),
        );
        q.append(&mut queued);
        queued = q;
    }
    let avail = h.saturating_sub(input_h + status_h);
    let mut queued_h = (queued.len() as u16).min(avail);
    let mut plan_h = plan.len() as u16;
    if plan_h + queued_h + min_trans > avail {
        plan_h = plan_h.min(avail.saturating_sub(queued_h + min_trans));
        if plan_h + queued_h + min_trans > avail {
            queued_h = queued_h.min(avail.saturating_sub(min_trans));
        }
    }
    let trans_h = avail.saturating_sub(plan_h + queued_h);
    let trans = Rect::new(body.x, body.y, body.width, trans_h).intersection(body);
    p.trans_rect = trans;
    draw_transcript(f, trans, ctx, p);
    let buf = f.buffer_mut();
    let plan_y = body.y + trans_h;
    for (i, l) in plan.iter().take(plan_h as usize).enumerate() {
        buf.set_line(col.x, plan_y + i as u16, l, col.width);
    }
    let queue_y = plan_y + plan_h;
    let queue_first = queued.len().saturating_sub(queued_h as usize);
    for (i, l) in queued
        .iter()
        .skip(queue_first)
        .take(queued_h as usize)
        .enumerate()
    {
        buf.set_line(col.x, queue_y + i as u16, l, col.width);
    }
    let region = Rect::new(col.x, queue_y + queued_h, col.width, input_h).intersection(body);
    p.input_rect = region;
    if input_h > 0 {
        if let Some((title, style)) = card_title {
            draw_card(
                f,
                region,
                &t,
                style.remove_modifier(Modifier::BOLD),
                (title, style),
                &card,
            );
        } else {
            draw_composer(f, region, ctx, p, focused, &lay, text_ask.is_some());
        }
    }
    let status = Rect::new(col.x, body.bottom().saturating_sub(1), col.width, 1);
    draw_status(f, status, ctx, p, focused);
}

fn is_text_ask(a: &BoardAsk) -> bool {
    matches!(a.input, AskInput::Text { .. })
}

/// What the empty message box says.
fn placeholder(p: &Pane, answering: bool) -> &'static str {
    if answering {
        "Type your answer…"
    } else if p.editing_queued.is_some() {
        "Edit the queued message…"
    } else if p.is_dm() {
        "A private conversation between bots: read-only"
    } else if p.busy() {
        "Add a message (it waits its turn) · esc interrupts"
    } else {
        "Ask anything, or describe a task…"
    }
}

/// Key hints: the key a shade brighter than what it does.
pub(super) fn hints(t: &Theme, items: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, (k, d)) in items.iter().enumerate() {
        if i > 0 {
            out.push(span("  ", Style::new()));
        }
        out.push(span(k.to_string(), t.key()));
        if !d.is_empty() {
            out.push(span(format!(" {d}"), t.faint()));
        }
    }
    out
}

/// The message box: a rounded box that grows with the text, with what ⏎
/// does in its bottom edge.
fn draw_composer(
    f: &mut Frame,
    r: Rect,
    ctx: &Ctx<'_>,
    p: &Pane,
    focused: bool,
    layout: &super::editor::Layout,
    answering: bool,
) {
    let t = ctx.theme;
    let active = focused && !ctx.modal;
    let border = if active { t.border(true) } else { t.hairline() };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border);
    let hint: Vec<Span<'static>> = if answering {
        hints(&t, &[("⏎", "answer"), ("ctrl+x", "decline")])
    } else if p.editing_queued.is_some() {
        hints(&t, &[("⏎", "save"), ("esc", "cancel")])
    } else if p.busy() && !p.editor.is_empty() {
        hints(&t, &[("⏎", "queue"), ("alt+⏎", "steer")])
    } else if !p.editor.is_empty() && !ctx.kbd_enhanced {
        hints(&t, &[("ctrl+j", "new line"), ("⏎", "send")])
    } else if p.editor.is_empty() && !p.busy() && !p.transcript.is_empty() {
        // (An empty chat's welcome lists these keys already.)
        hints(&t, &[("/", "commands"), ("@", "files")])
    } else if p.editor.is_empty() {
        Vec::new()
    } else {
        hints(&t, &[("⏎", "send")])
    };
    let hw = text::line_width(&Line::from(hint.clone()));
    if active && hw > 0 && (r.width as usize) > hw + 8 {
        let mut h = vec![span(" ", Style::new())];
        h.extend(hint);
        h.push(span(" ", Style::new()));
        block = block.title_bottom(Line::from(h).right_aligned());
    }
    if p.editing_queued.is_some() {
        block = block.title(Line::from(span(
            " Editing a queued message ",
            t.accent_text(),
        )));
    }
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.width < 3 || inner.height == 0 {
        return;
    }
    let text_x = inner.x + 3;
    let text_w = inner.width.saturating_sub(4);
    let visible = inner.height as usize;
    let scroll = if layout.cursor_row >= visible {
        layout.cursor_row + 1 - visible
    } else {
        0
    };
    let buf = f.buffer_mut();
    let prompt = if active { t.accent() } else { t.faint() };
    buf.set_line(inner.x + 1, inner.y, &Line::from(span("›", prompt)), 1);
    if p.editor.is_empty() {
        buf.set_line(
            text_x,
            inner.y,
            &Line::from(span(placeholder(p, answering), t.faint())),
            text_w,
        );
    } else {
        let src = p.editor.text();
        for (i, row) in layout.rows.iter().enumerate().skip(scroll).take(visible) {
            let y = inner.y + (i - scroll) as u16;
            buf.set_line(
                text_x,
                y,
                &Line::from(span(src[row.start..row.end].to_string(), t.text())),
                text_w,
            );
        }
        if scroll > 0 {
            buf.set_line(
                inner.right().saturating_sub(1),
                inner.y,
                &Line::from(span("↑", t.faint())),
                1,
            );
        }
    }
    if active && p.queue_sel.is_none() {
        let cy = inner.y + (layout.cursor_row.saturating_sub(scroll)) as u16;
        let cx = text_x + (layout.cursor_col as u16).min(text_w);
        if cy < inner.bottom() {
            f.set_cursor_position((cx, cy));
        }
    }
}

/// A rounded card (an approval, a question) in place of the message box.
fn draw_card(
    f: &mut Frame,
    r: Rect,
    t: &Theme,
    border: Style,
    title: (String, Style),
    lines: &[Line<'static>],
) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(span(title.0, title.1)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let buf = f.buffer_mut();
    if t.surfaces() {
        buf.set_style(inner, t.raised());
    }
    for (i, l) in lines.iter().take(inner.height as usize).enumerate() {
        buf.set_line(
            inner.x + 2,
            inner.y + i as u16,
            l,
            inner.width.saturating_sub(3),
        );
    }
}

// ---- the conversation ----------------------------------------------------------------

/// The top of an empty chat: who it is, where, and the few keys to know.
fn welcome(ctx: &Ctx<'_>, p: &Pane, width: usize) -> Vec<Line<'static>> {
    let t = &ctx.theme;
    let mut out = vec![Line::default()];
    let head = |words: &str| {
        Line::from(vec![
            span("  ", Style::new()),
            motif::logo(t),
            span("  Tessera", t.bold()),
            span(format!("  {words}"), t.dim()),
        ])
    };
    let Some(s) = &p.session else {
        if p.loading.is_some() {
            out.push(head(""));
            let mut l = vec![span("       ", Style::new())];
            l.extend(ctx.spinner());
            l.push(span(" Opening your chat…", t.dim()));
            out.push(Line::from(l));
            return out;
        }
        match ctx.boot {
            Boot::NoModel => {
                out.push(head(""));
                out.push(Line::default());
                out.push(Line::from(vec![
                    span("       ", Style::new()),
                    span("■ ", t.warning()),
                    span("No model is set up yet.", t.bold()),
                ]));
                for l in [
                    "Run `tessera setup`: it finds models on this computer and your network,",
                    "or signs you in to a plan you already have. Then start tessera again.",
                ] {
                    out.extend(text::wrap_spans(
                        &text::code_spans(l, t.muted(), t.accent_text()),
                        width,
                        &[span("       ", Style::new())],
                        &[span("       ", Style::new())],
                    ));
                }
            }
            _ => {
                out.push(head(""));
                let label = if ctx.boot == Boot::WaitingForModel {
                    "Looking for a model on this computer…"
                } else {
                    "Starting…"
                };
                let mut l = vec![span("       ", Style::new())];
                l.extend(ctx.spinner());
                l.push(span(format!(" {label}"), t.dim()));
                out.push(Line::from(l));
            }
        }
        return out;
    };
    out.push(head("a coding agent in your terminal"));
    let model = model_name(s, ctx.providers);
    out.push(text::truncate_line(
        Line::from(vec![
            span("       ", Style::new()),
            span(model, t.muted()),
            span("  ·  ", t.faint()),
            span(super::app::short_path(&s.cwd), t.muted()),
        ]),
        width,
    ));
    out.push(Line::default());
    let mut keys = vec![span("       ", Style::new())];
    keys.extend(hints(
        t,
        &[
            ("/", "commands"),
            ("@", "files"),
            ("shift+tab", "thinking"),
            ("?", "help"),
        ],
    ));
    out.push(text::truncate_line(Line::from(keys), width));
    out
}

/// Byte offset of the last paragraph break outside a code fence: text
/// before it is stable while a message streams.
fn stable_split(s: &str) -> usize {
    let mut in_fence: Option<&str> = None;
    let mut pos = 0;
    let mut last = 0;
    let mut prev_blank = false;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence && line.len() - trimmed.len() <= 3 {
            let marker = &trimmed[..3];
            match in_fence {
                None => in_fence = Some(marker),
                Some(m) if m == marker => in_fence = None,
                _ => {}
            }
        }
        let blank = line.trim().is_empty();
        if in_fence.is_none()
            && prev_blank
            && !blank
            && !line.starts_with(' ')
            && !line.starts_with('\t')
        {
            last = pos;
        }
        prev_blank = blank && in_fence.is_none();
        pos += line.len();
    }
    last
}

/// The streaming message, cached per pane: only a pane whose stream moved
/// (or whose indicator steps) is re-rendered.
fn live_lines(
    live: &Live,
    cache: &mut LiveCache,
    env: &Env<'_>,
    header: Option<(&Speaker, bool)>,
    think: (Option<Instant>, Option<u64>),
    after_row: bool,
) -> Vec<Line<'static>> {
    let mut h = DefaultHasher::new();
    (
        live.version,
        env.width,
        env.expand,
        env.thinking,
        live.ended,
        env.theme.cache_key(),
        think.1,
        after_row,
    )
        .hash(&mut h);
    if let Some((sp, replying)) = header {
        (sp, replying).hash(&mut h);
    }
    let last = live.content.len().saturating_sub(1);
    let thinking_now =
        !live.ended && matches!(live.content.last(), Some(AssistantPart::Thinking { .. }));
    let spinning = !live.ended
        && live.content.iter().any(|p| {
            matches!(
                p,
                AssistantPart::ToolCall { .. } | AssistantPart::Thinking { .. }
            )
        });
    if spinning {
        env.frame.hash(&mut h);
    }
    if thinking_now {
        env.now
            .saturating_duration_since(think.0.unwrap_or(env.now))
            .as_secs()
            .hash(&mut h);
    }
    let key = h.finish();
    if cache.key == key && !cache.lines.is_empty() {
        return cache.lines.clone();
    }
    // (a row?, lines)
    let mut blocks: Vec<(bool, Vec<Line<'static>>)> = Vec::new();
    for (i, p) in live.content.iter().enumerate() {
        match p {
            AssistantPart::Thinking { text: th, .. } => {
                if !th.trim().is_empty() {
                    let live_now = i == last && !live.ended;
                    let lines = transcript::live_thinking(
                        th,
                        if live_now { think.0 } else { None },
                        if live_now { None } else { think.1 },
                        env,
                    );
                    blocks.push((!env.thinking, lines));
                }
            }
            AssistantPart::Text { text: tx } => {
                if tx.trim().is_empty() {
                    continue;
                }
                let width = env.width.saturating_sub(2);
                let split = stable_split(tx);
                let mut lines = Vec::new();
                if split > 0 {
                    let reuse = matches!(&cache.prefix, Some((pi, sp, w, _)) if *pi == i && *sp == split && *w == width);
                    if !reuse {
                        let pre = markdown::render(&tx[..split], width, env.theme);
                        cache.prefix = Some((i, split, width, pre));
                    }
                    if let Some((_, _, _, pre)) = &cache.prefix {
                        lines.extend(pre.iter().cloned());
                    }
                    lines.push(Line::default());
                    lines.extend(markdown::render(&tx[split..], width, env.theme));
                } else {
                    lines = markdown::render(tx, width, env.theme);
                }
                blocks.push((
                    false,
                    lines
                        .into_iter()
                        .map(|mut l| {
                            if !l.spans.is_empty() {
                                l.spans.insert(0, Span::raw("  "));
                            }
                            l
                        })
                        .collect(),
                ));
            }
            AssistantPart::ToolCall {
                name, arguments, ..
            } => {
                blocks.push((true, transcript::streaming_call(name, arguments, env)));
            }
        }
    }
    let mut out = Vec::new();
    if let Some((sp, replying)) = header {
        out.push(Line::default());
        out.push(transcript::speaker_line(
            sp,
            env,
            replying.then_some("is replying…"),
        ));
    }
    // Rows stack under rows; prose gets a blank line.
    let mut prev: Option<bool> = if header.is_some() {
        Some(false)
    } else if after_row {
        Some(true)
    } else {
        None
    };
    let mut first = true;
    for (row, b) in blocks {
        let gap = match prev {
            Some(true) => !row,
            Some(false) => !(first && header.is_some()),
            None => true,
        };
        if gap {
            out.push(Line::default());
        }
        out.extend(b);
        prev = Some(row);
        first = false;
    }
    cache.key = key;
    cache.lines = out.clone();
    out
}

fn draw_transcript(f: &mut Frame, r: Rect, ctx: &Ctx<'_>, p: &mut Pane) {
    if r.height == 0 || r.width < 3 {
        return;
    }
    let col = column(r);
    let width = (col.width as usize).saturating_sub(2).max(1);
    let now = ctx.now;
    p.page = (r.height as usize / 2).max(1);
    let active = p.busy();
    let channel = p.is_channel();
    let interrupted = p
        .session
        .as_ref()
        .is_some_and(|s| s.status == SessionStatus::Interrupted);
    let bot_id: Option<String> = p.bot_id().map(str::to_string);
    let cwd: String = p
        .session
        .as_ref()
        .map(|s| s.cwd.clone())
        .unwrap_or_default();
    let env = Env {
        theme: &ctx.theme,
        width,
        expand: ctx.expand,
        thinking: ctx.thinking,
        frame: ctx.step,
        running: &p.running,
        approvals: &p.approvals,
        active,
        now,
        children: &p.children,
        cwd: &cwd,
        interrupted,
        bots: ctx.bots,
        bot: bot_id.as_deref(),
        channel,
        think_ms: &p.think_ms,
    };
    let total_items = p.transcript.prepare(&env);
    let after_row = p.transcript.ends_with_row();
    // A quiet answer to a completion check (a ✓, ledger updates) streams
    // unseen, as it folds away once it's in.
    let quiet = |l: &Live| {
        p.live_author.as_deref() == Some(tessera_core::tools::meta::CHECK_AUTHOR)
            || tessera_core::tools::meta::check_ack_parts(&l.content)
            || (l.content.iter().all(transcript::quiet_part) && p.transcript.quiet_check_open())
    };
    let live = match p.live.as_ref() {
        Some(l) if !quiet(l) => {
            let author = p.live_author.clone().or(bot_id.clone());
            let labelled = channel || bot_id.is_some();
            let sp = author.map(Speaker::Bot);
            let header = match &sp {
                Some(s) if labelled && p.transcript.last_speaker() != Some(s) => {
                    Some((s, channel && !l.ended))
                }
                _ => None,
            };
            live_lines(
                l,
                &mut p.live_cache,
                &env,
                header,
                (p.think_started, p.think_live_ms),
                after_row,
            )
        }
        _ => Vec::new(),
    };
    let intro: Vec<Line<'static>> = if p.transcript.is_empty() && live.is_empty() {
        welcome(ctx, p, width)
    } else {
        Vec::new()
    };
    let trailer: Vec<Line<'static>> =
        if active && live.is_empty() && p.running.is_empty() && p.approvals.is_empty() {
            let label = match p.session.as_ref().map(|s| s.status) {
                Some(SessionStatus::Compacting) => "Summarizing the conversation…",
                _ => "Working…",
            };
            let mut l = vec![span("  ", Style::new())];
            l.extend(ctx.spinner());
            l.push(span(format!(" {label}"), ctx.theme.dim()));
            vec![Line::default(), Line::from(l)]
        } else {
            vec![Line::default()]
        };
    let total = intro.len() + total_items + live.len() + trailer.len();

    // Keep the viewport anchored while content grows below it.
    if p.scroll > 0 && width == p.last_width && total > p.last_total {
        p.scroll += total - p.last_total;
    }
    p.last_total = total;
    p.last_width = width;
    let h = r.height as usize;
    let max_scroll = total.saturating_sub(h);
    p.scroll = p.scroll.min(max_scroll);
    let start = total.saturating_sub(h + p.scroll);

    let buf = f.buffer_mut();
    let mut y = 0usize;
    let mut idx = 0usize;
    let blocks = std::iter::once(intro.as_slice())
        .chain(p.transcript.blocks())
        .chain([live.as_slice(), trailer.as_slice()]);
    'outer: for block in blocks {
        let len = block.len();
        if idx + len <= start {
            idx += len;
            continue;
        }
        let from = start.saturating_sub(idx);
        for line in &block[from..] {
            if y >= h {
                break 'outer;
            }
            buf.set_line(col.x + 1, r.y + y as u16, line, width as u16);
            y += 1;
        }
        idx += len;
    }
    if p.scroll > 0 && p.completion.is_none() {
        let label = format!(
            " ↓ {} more line{} · ctrl+end ",
            p.scroll,
            if p.scroll == 1 { "" } else { "s" }
        );
        let label = text::truncate(&label, r.width.saturating_sub(2) as usize);
        let lw = text::width(&label) as u16;
        let x = col.right().saturating_sub(lw + 1);
        let style = ctx.theme.selected().patch(ctx.theme.accent_text());
        buf.set_line(x, r.bottom() - 1, &Line::from(span(label, style)), lw);
    }
}

// ---- plan & queue ------------------------------------------------------------------

/// The agent's plan, above the message box: its goal, a strip of steps and
/// the few around the current one (one line with ctrl+l, or side by side).
fn plan_lines(p: &Pane, ctx: &Ctx<'_>, width: usize) -> Vec<Line<'static>> {
    let t = &ctx.theme;
    let Some(task) = p.session.as_ref().and_then(|s| s.task.as_ref()) else {
        return Vec::new();
    };
    if task.goal.trim().is_empty() && task.plan.is_empty() {
        return Vec::new();
    }
    let done = task
        .plan
        .iter()
        .filter(|p| matches!(p.status, PlanStatus::Done | PlanStatus::Skipped))
        .count();
    let total = task.plan.len();
    let current = task
        .plan
        .iter()
        .position(|p| p.status == PlanStatus::InProgress)
        .or_else(|| {
            task.plan
                .iter()
                .position(|p| p.status == PlanStatus::Pending)
        });
    let goal = if task.goal.trim().is_empty() {
        "Plan".to_string()
    } else {
        text::sanitize(&tessera_core::util::one_line(&task.goal, 400)).into_owned()
    };
    let states: Vec<Sq> = task
        .plan
        .iter()
        .map(|p| match p.status {
            PlanStatus::Done => Sq::Done,
            PlanStatus::InProgress => Sq::Running,
            PlanStatus::Blocked => Sq::Failed,
            _ => Sq::Waiting,
        })
        .collect();
    let right = |strip: bool| {
        let mut r = Vec::new();
        if strip && total > 0 {
            r.extend(motif::strip(t, &states, 12));
            r.push(span(" ", Style::new()));
        }
        if total > 0 {
            r.push(span(format!("{done}/{total} "), t.dim()));
        }
        r
    };
    let mut out = Vec::new();
    if ctx.plan_collapsed || ctx.several {
        let mut s = goal;
        if let Some(c) = current {
            s.push_str(&format!(
                "  ▸ {}",
                text::sanitize(&tessera_core::util::one_line(&task.plan[c].text, 200))
            ));
        }
        let left = vec![span("  ◆ ", t.accent()), span(s, t.muted())];
        out.push(header_line(left, right(false), width));
        return out;
    }
    let left = vec![span("  ◆ ", t.accent()), span(goal, t.bold())];
    // A line of air between the conversation and the plan.
    out.push(Line::default());
    out.push(header_line(left, right(true), width));
    if total == 0 {
        return out;
    }
    let max_items = 5usize;
    let center = current.unwrap_or(total.saturating_sub(1));
    let start = center
        .saturating_sub(1)
        .min(total.saturating_sub(max_items));
    let end = (start + max_items).min(total);
    if start > 0 {
        out.push(Line::from(span(
            format!("      … {start} done above"),
            t.faint(),
        )));
    }
    for (i, item) in task.plan[start..end].iter().enumerate() {
        let ts = match item.status {
            PlanStatus::Done => t.dim(),
            PlanStatus::InProgress => t.bold(),
            PlanStatus::Pending => t.muted(),
            PlanStatus::Blocked => t.error(),
            PlanStatus::Skipped => t.faint().add_modifier(Modifier::CROSSED_OUT),
        };
        let line = Line::from(vec![
            span("    ", Style::new()),
            motif::square(t, states[start + i]),
            span(" ", Style::new()),
            span(
                text::sanitize(&tessera_core::util::one_line(&item.text, 300)).into_owned(),
                ts,
            ),
        ]);
        out.push(text::truncate_line(line, width.saturating_sub(1)));
    }
    if end < total {
        out.push(Line::from(span(
            format!("      … {} more", total - end),
            t.faint(),
        )));
    }
    out
}

/// `left` and `right` on one line of `width` cells (left truncated first).
fn header_line(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rw = text::line_width(&Line::from(right.clone()));
    if rw + 4 > width {
        return text::truncate_line(Line::from(left), width);
    }
    let left = text::truncate_line(Line::from(left), width - rw - 1);
    let lw = text::line_width(&left);
    let mut spans = left.spans;
    spans.push(span(" ".repeat(width - lw - rw), Style::new()));
    spans.extend(right);
    Line::from(spans)
}

/// Messages waiting for the agent, above the message box: "Queued" (after
/// this reply), "Steering" (read at the next step) or "Sending" (the agent
/// stops for it). ↑ on an empty message box selects one.
fn queue_lines(p: &Pane, t: &Theme, width: usize, focused: bool) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if p.queued.is_empty() {
        return out;
    }
    let n = p.queued.len();
    let sel = p.queue_sel.filter(|_| focused);
    // At most three rows, keeping the selection in view.
    let first = match sel {
        Some(s) if s >= 3 => s - 2,
        _ => 0,
    };
    if first > 0 {
        out.push(Line::from(span(
            format!("    … {first} earlier"),
            t.faint(),
        )));
    }
    for (i, q) in p.queued.iter().enumerate().skip(first).take(3) {
        let selected = sel == Some(i);
        let (tag, tag_style) = if q.interrupt {
            ("sending ", t.accent_text())
        } else if q.steer {
            ("steering", t.accent_text())
        } else {
            ("queued  ", t.dim())
        };
        let left = vec![
            span(if selected { "  › " } else { "    " }, t.accent()),
            span(tag, tag_style),
            span("  ", Style::new()),
            span(
                text::sanitize(&q.text).replace('\n', " "),
                if selected { t.text() } else { t.muted() },
            ),
        ];
        let right = if selected && !q.interrupt {
            let mut h = if q.steer {
                hints(t, &[("e", "edit"), ("n", "send now"), ("x", "remove")])
            } else {
                hints(
                    t,
                    &[
                        ("e", "edit"),
                        ("s", "steer"),
                        ("n", "send now"),
                        ("x", "remove"),
                    ],
                )
            };
            h.push(span(" ", Style::new()));
            h
        } else if i == n - 1 && sel.is_none() && focused && p.editor.is_empty() {
            let mut h = hints(t, &[("↑", "select")]);
            h.push(span(" ", Style::new()));
            h
        } else {
            Vec::new()
        };
        let mut line = header_line(left, right, width);
        if selected {
            line = line.patch_style(t.selected());
        }
        out.push(line);
    }
    let shown = first + 3;
    if n > shown {
        out.push(Line::from(span(
            format!("    … and {} more", n - shown),
            t.faint(),
        )));
    }
    out
}

/// All of what is being approved: nothing cut off (a long one scrolls).
fn approval_lines(req: &ApprovalRequest, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    // Spelled out, not sanitized: sanitizing applies `\r`, which could hide
    // the start of the command being approved.
    let display =
        tessera_proto::presentation::ToolPresentation::for_call(&req.tool, &req.arguments);
    let summary = tessera_core::util::visible_controls(&req.summary);
    let first = [span(display.kind, t.bold()), span("  ", Style::new())];
    let cont = [span("  ", Style::new())];
    out.extend(text::wrap_plain(&summary, t.text(), width, &first, &cont));
    if !req.reason.trim().is_empty() {
        let reason = tessera_core::util::visible_controls(req.reason.trim());
        out.extend(text::wrap_spans(
            &text::code_spans(&reason, t.dim(), t.accent_text()),
            width,
            &[],
            &[],
        ));
    }
    out
}

/// The answer keys under an approval (and how much is left to read).
fn approval_keys(
    req: &ApprovalRequest,
    n: usize,
    v: &ApprovalView,
    width: usize,
    t: &Theme,
) -> Line<'static> {
    // The answer keys are the point of the card: bold keys, readable labels.
    let mut keys = Vec::new();
    if !v.read {
        // Counted from what was shown without a gap, wherever it is scrolled.
        let left = v.total.saturating_sub(v.seen);
        keys.push(span("↓", t.bold()));
        keys.push(span(
            format!(
                " {left} more line{} to read   ",
                if left == 1 { "" } else { "s" }
            ),
            t.warning(),
        ));
    } else if v.scroll > 0 {
        keys.push(span("↑ back up   ", t.faint()));
    }
    // A machine that asks every time has no "for this chat".
    let answers: &[(&str, &str)] = if req.once {
        &[("y", "allow"), ("n", "deny")]
    } else {
        &[("y", "allow"), ("a", "allow for this chat"), ("n", "deny")]
    };
    for (i, (k, d)) in answers.iter().enumerate() {
        if i > 0 {
            keys.push(span("   ", Style::new()));
        }
        keys.push(span(k.to_string(), t.bold()));
        keys.push(span(format!(" {d}"), t.muted()));
    }
    if n > 1 {
        keys.push(span(format!("   +{} more", n - 1), t.faint()));
    }
    text::truncate_line(Line::from(keys), width)
}

// ---- status line ---------------------------------------------------------------------

fn fmt_turn(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// The model's display name, as the providers know it.
fn model_name(s: &SessionInfo, providers: &[ProviderInfo]) -> String {
    providers
        .iter()
        .find(|p| p.id == s.model.provider)
        .and_then(|p| p.models.iter().find(|m| m.id == s.model.model))
        .map(|m| m.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| s.model.model.clone())
}

/// What the chat is doing, in a few words.
fn state_spans(ctx: &Ctx<'_>, p: &Pane) -> Vec<Span<'static>> {
    let t = &ctx.theme;
    if ctx.conn != ConnState::Connected {
        let mut v = ctx.spinner();
        v.push(span(" reconnecting", t.warning()));
        return v;
    }
    let Some(s) = &p.session else {
        return match ctx.boot {
            Boot::NoModel => vec![span("■ ", t.warning()), span("no model", t.warning())],
            _ => {
                let mut v = ctx.spinner();
                v.push(span(" starting", t.dim()));
                v
            }
        };
    };
    let elapsed = p
        .turn_started
        .map(|st| st.elapsed().as_secs())
        .filter(|s| *s >= 1)
        .map(|s| format!(" {}", fmt_turn(s)))
        .unwrap_or_default();
    let busy = |label: String| {
        let mut v = ctx.spinner();
        v.push(span(format!(" {label}"), t.muted()));
        v.push(span(elapsed.clone(), t.faint()));
        v
    };
    if ask::pending_ask(p, ctx.boards).is_some() {
        return vec![
            span("■ ", t.warning()),
            span("waiting for your answer", t.warning()),
        ];
    }
    match s.status {
        SessionStatus::AwaitingApproval => {
            vec![
                span("■ ", t.warning()),
                span("needs your approval", t.warning()),
            ]
        }
        SessionStatus::Compacting => busy("summarizing".into()),
        SessionStatus::Running => {
            let label = if !p.running.is_empty() {
                let mut names: Vec<&str> = p.running.values().map(|r| r.name.as_str()).collect();
                names.sort();
                names.dedup();
                if p.running.len() == 1 {
                    format!("running {}", names[0])
                } else {
                    format!("running {} tools", p.running.len())
                }
            } else {
                match p.live.as_ref().and_then(|l| {
                    l.content
                        .iter()
                        .rev()
                        .find(|p| !matches!(p, AssistantPart::Text { text } if text.is_empty()))
                }) {
                    Some(AssistantPart::Text { .. }) => "writing".to_string(),
                    Some(AssistantPart::ToolCall { name, .. }) => format!("preparing {name}"),
                    _ => "thinking".to_string(),
                }
            };
            busy(label)
        }
        SessionStatus::Error => vec![span("■ ", t.error()), span("error", t.error())],
        SessionStatus::Interrupted => {
            vec![span("■ ", t.warning()), span("interrupted", t.dim())]
        }
        SessionStatus::Idle => vec![span("■ ", t.success()), span("ready", t.dim())],
    }
}

/// How full the context is: a slim bar and the numbers.
fn context_meter(s: &SessionInfo, providers: &[ProviderInfo], t: &Theme) -> Vec<Span<'static>> {
    // The engine refreshes `context.window` only on the next request; the
    // provider list knows the new model's window right away.
    let window = providers
        .iter()
        .find(|p| p.id == s.model.provider)
        .and_then(|p| p.models.iter().find(|m| m.id == s.model.model))
        .map(|m| m.context_window)
        .filter(|w| *w > 0)
        .unwrap_or(s.context.window);
    if window == 0 {
        return Vec::new();
    }
    let ratio = s.context.tokens as f64 / window as f64;
    let mut v = motif::bar(t, ratio, 6);
    v.push(span(
        format!(
            " {}/{}",
            text::tokens(s.context.tokens),
            text::tokens(window)
        ),
        t.faint(),
    ));
    v
}

/// The line under the message box: what the chat is doing, its model,
/// folder and context; a notice or key hint on the right.
fn draw_status(f: &mut Frame, r: Rect, ctx: &Ctx<'_>, p: &Pane, focused: bool) {
    let t = &ctx.theme;
    // (priority: higher drops first, spans)
    let mut items: Vec<(u8, Vec<Span<'static>>)> = vec![(0, state_spans(ctx, p))];
    if let Some(s) = &p.session {
        items.push((3, vec![span(model_name(s, ctx.providers), t.muted())]));
        items.push((
            4,
            vec![span(super::app::compact_path(&s.cwd, 32), t.faint())],
        ));
        let meter = context_meter(s, ctx.providers, t);
        if !meter.is_empty() {
            items.push((2, meter));
        }
        match s.permission {
            PermissionMode::Auto => items.push((
                1,
                vec![span(format!("{} YOLO", super::theme::YOLO), t.yolo())],
            )),
            PermissionMode::Ask | PermissionMode::ReadOnly => items.push((
                1,
                vec![span(perm_label(s.permission).to_string(), t.accent_text())],
            )),
            PermissionMode::Default => {}
        }
        if ctx.thinking || s.thinking != ThinkingLevel::Auto {
            items.push((
                5,
                vec![span(
                    format!("think {}", thinking_name(s.thinking)),
                    t.faint(),
                )],
            ));
        }
    }
    if !p.queued.is_empty() {
        items.push((
            2,
            vec![span(format!("{} queued", p.queued.len()), t.accent_text())],
        ));
    }
    let (hint, is_notice): (Vec<Span<'static>>, bool) = match (&ctx.notice, focused) {
        (Some((text_s, style)), true) => (vec![span(text_s.clone(), *style)], true),
        _ if !focused => (Vec::new(), false),
        _ if !p.approvals.is_empty() => (Vec::new(), false),
        _ if p.queue_sel.is_some() => (hints(t, &[("↑↓", "select"), ("esc", "back")]), false),
        _ if p.busy() => (hints(t, &[("esc", "interrupt")]), false),
        _ if p.scroll > 0 => (hints(t, &[("ctrl+end", "latest")]), false),
        _ => (hints(t, &[("?", "help")]), false),
    };
    let width = r.width as usize;
    let build = |items: &[(u8, Vec<Span<'static>>)]| -> Vec<Span<'static>> {
        let mut out = vec![span("  ", Style::new())];
        for (i, (_, s)) in items.iter().enumerate() {
            if i > 0 {
                out.push(span("  ·  ", t.faint()));
            }
            out.extend(s.iter().cloned());
        }
        out
    };
    let hw = text::line_width(&Line::from(hint.clone()));
    let fits = |kept: &[(u8, Vec<Span<'static>>)], with_hint: bool| {
        let lw = text::line_width(&Line::from(build(kept)));
        lw + if with_hint && hw > 0 { hw + 4 } else { 1 } <= width
    };
    let mut kept = items;
    let mut show_hint = true;
    while !fits(&kept, show_hint) {
        // A key hint goes before the model and the bar; a notice stays.
        if show_hint && !is_notice && kept.iter().all(|(p, _)| *p <= 3) {
            show_hint = false;
            continue;
        }
        let Some(idx) = kept
            .iter()
            .enumerate()
            .filter(|(_, (p, _))| *p > 0)
            .max_by_key(|(_, (p, _))| *p)
            .map(|(i, _)| i)
        else {
            break;
        };
        kept.remove(idx);
    }
    let buf = f.buffer_mut();
    if is_notice && !fits(&kept, true) {
        // A long notice wins the whole line.
        let room = width.saturating_sub(2);
        let line = text::truncate_line(Line::from(hint), room);
        buf.set_line(r.x + 2, r.y, &line, room as u16);
        return;
    }
    let left = text::truncate_line(Line::from(build(&kept)), width);
    buf.set_line(r.x, r.y, &left, r.width);
    if show_hint && hw > 0 && hw + 2 < width {
        let x = r.right().saturating_sub(hw as u16 + 1);
        buf.set_line(x, r.y, &Line::from(hint), hw as u16);
    }
}

// ---- popups --------------------------------------------------------------------------

/// A popup: a rounded edge, a bold title and key hints in the bottom edge.
/// Returns the inside.
pub(super) fn popup(
    f: &mut Frame,
    rect: Rect,
    t: &Theme,
    title: &str,
    hint: &[(&str, &str)],
) -> Rect {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(t.hairline())
        .style(t.raised());
    if !title.is_empty() {
        block = block.title(Line::from(vec![
            span(" ", Style::new()),
            span(title.to_string(), t.title()),
            span(" ", Style::new()),
        ]));
    }
    if !hint.is_empty() {
        let mut h = vec![span(" ", Style::new())];
        h.extend(hints(t, hint));
        h.push(span(" ", Style::new()));
        let hw = text::line_width(&Line::from(h.clone()));
        if hw + 4 <= rect.width as usize {
            block = block.title_bottom(Line::from(h).right_aligned());
        }
    }
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    inner
}

fn draw_completion(f: &mut Frame, t: &Theme, p: &Pane) {
    let Some(c) = &p.completion else { return };
    let (input, trans) = (p.input_rect, p.trans_rect);
    if c.items.is_empty() || input.height == 0 || trans.height == 0 {
        return;
    }
    let max_rows = 10usize
        .min((trans.height as usize).saturating_sub(2))
        .max(1);
    let n = c.items.len().min(max_rows);
    let label_w = c
        .items
        .iter()
        .map(|i| text::width(&i.label))
        .max()
        .unwrap_or(4)
        .min(40);
    let detail_w = c
        .items
        .iter()
        .map(|i| text::width(&i.detail))
        .max()
        .unwrap_or(0);
    let want = (label_w + 2 + detail_w + 7).clamp(24, 96);
    let width = (want as u16).min(input.width);
    let height = n as u16 + 2;
    let y = input.y.saturating_sub(height).max(trans.y);
    let rect = Rect::new(
        input.x,
        y,
        width,
        height.min(input.y.saturating_sub(trans.y)).max(3),
    )
    .intersection(f.area());
    if rect.height < 3 || rect.width < 8 {
        return;
    }
    let title = match c.kind {
        CompKind::Command => "Commands",
        CompKind::Arg => "Options",
        CompKind::File => "Files",
    };
    let inner = popup(f, rect, t, title, &[]);
    let rows = inner.height as usize;
    let first = if c.selected >= rows {
        c.selected + 1 - rows
    } else {
        0
    };
    let buf = f.buffer_mut();
    for (i, item) in c.items.iter().enumerate().skip(first).take(rows) {
        let y = inner.y + (i - first) as u16;
        let selected = i == c.selected;
        let base = if selected { t.selected() } else { Style::new() };
        if selected {
            buf.set_style(Rect::new(inner.x, y, inner.width, 1), base);
        }
        let label_style = if selected {
            t.accent_text().add_modifier(Modifier::BOLD)
        } else {
            t.text()
        };
        let mut spans = vec![span(if selected { " › " } else { "   " }, t.accent())];
        spans.push(span(
            format!("{:<label_w$}", text::truncate(&item.label, label_w)),
            label_style,
        ));
        if !item.detail.is_empty() {
            spans.push(span(format!("  {}", item.detail), t.dim()));
        }
        let line = text::truncate_line(Line::from(spans), inner.width as usize).patch_style(base);
        buf.set_line(inner.x, y, &line, inner.width);
    }
}

pub(super) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(10.min(area.width));
    let h = h.min(area.height.saturating_sub(2)).max(3.min(area.height));
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
    .intersection(area)
}

fn draw_modal(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let mut hits = super::app::ModalHits::default();
    match app.modal.as_mut() {
        Some(Modal::Info(info)) => {
            let width = area.width.saturating_sub(4).min(88);
            let inner_w = width.saturating_sub(6) as usize;
            let mut wrapped: Vec<Line<'static>> = Vec::new();
            for l in &info.lines {
                if text::line_width(l) <= inner_w {
                    wrapped.push(l.clone());
                } else {
                    // Continuation lines keep the line's indent, or line up
                    // under the text after a key column (`key   what it does`).
                    let first = l.spans.first().map(|s| s.content.as_ref()).unwrap_or("");
                    let indent = if l.spans.len() > 1 && first.ends_with("  ") {
                        text::width(first)
                    } else {
                        first.len() - first.trim_start_matches(' ').len()
                    }
                    .min(inner_w / 3);
                    let cont = [span(" ".repeat(indent), Style::new())];
                    wrapped.extend(text::wrap_spans(&l.spans, inner_w, &[], &cont));
                }
            }
            let height = (wrapped.len() as u16 + 4).min(area.height.saturating_sub(2));
            let rect = centered(area, width, height);
            hits.popup = rect;
            let inner = popup(f, rect, &t, &info.title, &[("esc", "close")]);
            let rows = inner.height.saturating_sub(2) as usize;
            let max_scroll = wrapped.len().saturating_sub(rows);
            info.scroll = info.scroll.min(max_scroll);
            let buf = f.buffer_mut();
            for (i, l) in wrapped.iter().skip(info.scroll).take(rows).enumerate() {
                buf.set_line(
                    inner.x + 2,
                    inner.y + 1 + i as u16,
                    l,
                    inner.width.saturating_sub(3),
                );
            }
            if max_scroll > 0 {
                let label = format!(
                    " {}/{} ",
                    (info.scroll + rows).min(wrapped.len()),
                    wrapped.len()
                );
                let lw = text::width(&label) as u16;
                buf.set_line(
                    rect.right().saturating_sub(lw + 2),
                    rect.y,
                    &Line::from(span(label, t.faint())),
                    lw,
                );
            }
        }
        Some(Modal::Picker(p)) => draw_picker(f, area, p, &t, &mut hits),
        Some(Modal::Rewind(form)) => draw_rewind(f, area, form, &t),
        Some(Modal::Confirm(c)) => {
            let width = area.width.saturating_sub(4).min(68);
            let mut body: Vec<Line<'static>> = Vec::new();
            for para in c.text.split('\n') {
                body.extend(text::wrap_plain(
                    para,
                    t.muted(),
                    width.saturating_sub(6) as usize,
                    &[],
                    &[],
                ));
            }
            let rect = centered(area, width, body.len() as u16 + 4);
            hits.popup = rect;
            let inner = popup(f, rect, &t, &c.title, &[("y", "yes"), ("esc", "no")]);
            let buf = f.buffer_mut();
            for (i, l) in body.iter().enumerate() {
                buf.set_line(
                    inner.x + 2,
                    inner.y + 1 + i as u16,
                    l,
                    inner.width.saturating_sub(3),
                );
            }
        }
        None => {}
    }
    app.modal_hits = hits;
}

/// The rewind preview: what goes, what comes back, what can't be undone,
/// and the two choices.
fn draw_rewind(f: &mut Frame, area: Rect, form: &super::app::RewindForm, t: &Theme) {
    let width = area.width.saturating_sub(4).min(92);
    let iw = width.saturating_sub(6) as usize;
    let r = &form.result;
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(text::truncate_line(
        Line::from(vec![
            span("▎ ", t.accent()),
            span(
                text::sanitize(&tessera_core::util::one_line(&form.text, 300)).into_owned(),
                t.text(),
            ),
        ]),
        iw,
    ));
    lines.push(Line::default());
    let check = |on: bool, enabled: bool| {
        if !enabled {
            span("□ ", t.faint())
        } else if on {
            span("■ ", t.accent())
        } else {
            span("□ ", t.dim())
        }
    };
    let n = r.removed_entries;
    lines.push(Line::from(vec![
        check(form.conversation, true),
        span("Conversation", t.bold()),
        span(
            format!(
                "  removes this message and {} item{} after it",
                n.saturating_sub(1),
                if n.saturating_sub(1) == 1 { "" } else { "s" }
            ),
            t.dim(),
        ),
    ]));
    if form.conversation {
        lines.push(Line::from(span(
            "  The full chat is kept as a “(rewound)” copy; your message comes back to edit.",
            t.faint(),
        )));
    }
    let changes = form.file_changes();
    let fs = if changes == 0 {
        "  no file changes to put back".to_string()
    } else {
        let mut v = Vec::new();
        if !r.restored.is_empty() {
            v.push(format!("{} restored", r.restored.len()));
        }
        if !r.deleted.is_empty() {
            v.push(format!("{} deleted", r.deleted.len()));
        }
        format!("  {}", v.join(" · "))
    };
    lines.push(Line::from(vec![
        check(form.files, changes > 0),
        span("Files", if changes > 0 { t.bold() } else { t.dim() }),
        span(fs, t.dim()),
    ]));
    let file_style = if form.files { t.muted() } else { t.faint() };
    let list = |label: &str, items: &[String], out: &mut Vec<Line<'static>>| {
        for (i, p) in items.iter().enumerate() {
            if i == 5 {
                out.push(Line::from(span(
                    format!("      … {} more", items.len() - 5),
                    t.faint(),
                )));
                break;
            }
            out.push(text::truncate_line(
                Line::from(vec![
                    span(format!("    {label:<9}"), t.faint()),
                    span(text::sanitize(p).into_owned(), file_style),
                ]),
                iw,
            ));
        }
    };
    list("restore", &r.restored, &mut lines);
    list("delete", &r.deleted, &mut lines);
    for s in r.skipped.iter().take(5) {
        lines.push(text::truncate_line(
            Line::from(vec![
                span("    skipped  ", t.faint()),
                span(text::sanitize(&s.path).into_owned(), t.muted()),
                span(format!("  {}", text::sanitize(&s.reason)), t.faint()),
            ]),
            iw,
        ));
    }
    if !r.not_undone.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(vec![
            span("■ ", t.warning()),
            span("Not undone: shell commands", t.warning()),
            span("  (whatever they changed stays)", t.faint()),
        ]));
        for c in r.not_undone.iter().take(5) {
            lines.push(text::truncate_line(
                Line::from(vec![
                    span("    ", Style::new()),
                    span(
                        tessera_core::util::visible_controls(&tessera_core::util::one_line(c, 200)),
                        t.muted(),
                    ),
                ]),
                iw,
            ));
        }
        if r.not_undone.len() > 5 {
            lines.push(Line::from(span(
                format!("    … {} more", r.not_undone.len() - 5),
                t.faint(),
            )));
        }
    }
    for n in r.notes.iter().take(3) {
        lines.extend(text::wrap_plain(
            &text::sanitize(n),
            t.faint(),
            iw,
            &[span("▪ ", t.faint())],
            &[span("  ", Style::new())],
        ));
    }
    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);
    let mut hint: Vec<(&str, &str)> = vec![("c", "conversation")];
    if changes > 0 {
        hint.push(("f", "files"));
    }
    hint.extend([("⏎", "rewind"), ("esc", "cancel")]);
    let title = if form.running {
        "Rewinding…"
    } else {
        "Rewind to before this message"
    };
    let inner = popup(f, rect, t, title, &hint);
    let buf = f.buffer_mut();
    for (i, l) in lines
        .iter()
        .take(inner.height.saturating_sub(1) as usize)
        .enumerate()
    {
        buf.set_line(
            inner.x + 2,
            inner.y + 1 + i as u16,
            l,
            inner.width.saturating_sub(3),
        );
    }
}

fn draw_picker(
    f: &mut Frame,
    area: Rect,
    p: &mut Picker,
    t: &Theme,
    hits: &mut super::app::ModalHits,
) {
    let vis = p.visible();
    if p.selected >= vis.len() {
        p.selected = vis.len().saturating_sub(1);
    }
    let filter = p.filterable();
    // Rows: group headers (only when unfiltered) + items.
    let grouped = p.filter.trim().is_empty() && p.items.iter().any(|i| !i.group.is_empty());
    let mut rows: Vec<(Option<usize>, String)> = Vec::new();
    let mut last_group = String::new();
    for (vi, &ii) in vis.iter().enumerate() {
        let it = &p.items[ii];
        if grouped && it.group != last_group {
            if !rows.is_empty() {
                rows.push((None, String::new()));
            }
            rows.push((None, it.group.clone()));
            last_group = it.group.clone();
        }
        rows.push((Some(vi), String::new()));
    }
    let width = area.width.saturating_sub(4).min(96);
    let head = if filter { 2 } else { 1 };
    let list_h = (rows.len().max(1) as u16).clamp(1, area.height.saturating_sub(6 + head).max(1));
    let height = list_h + 3 + head;
    let rect = centered(area, width, height);
    let hint: &[(&str, &str)] = match p.kind {
        PickKind::Settings => &[("⏎", "change"), ("esc", "close")],
        _ if p.from_settings => &[("⏎", "choose"), ("esc", "back")],
        _ => &[("↑↓", "move"), ("⏎", "choose"), ("esc", "close")],
    };
    let inner = popup(f, rect, t, &p.title, hint);
    hits.popup = rect;
    if inner.height < 3 {
        return;
    }
    let buf = f.buffer_mut();
    let x = inner.x + 2;
    let iw = inner.width.saturating_sub(3) as usize;
    let mut list_y = inner.y + 1;
    if filter {
        let filter_line = if p.filter.is_empty() {
            Line::from(vec![
                span("⌕ ", t.faint()),
                span("Type to search", t.faint()),
            ])
        } else {
            Line::from(vec![
                span("⌕ ", t.accent()),
                span(p.filter.clone(), t.text()),
                span("▏", t.accent()),
            ])
        };
        buf.set_line(x, inner.y, &filter_line, iw as u16);
        list_y = inner.y + 2;
    }
    let list_rows = inner.bottom().saturating_sub(list_y) as usize;
    if p.loading {
        let mut l = motif::spinner(t, 0, 3);
        l.push(span(" Loading…", t.dim()));
        buf.set_line(x, list_y, &Line::from(l), iw as u16);
        return;
    }
    if vis.is_empty() {
        let msg = if !p.items.is_empty() {
            "Nothing matches".to_string()
        } else if p.empty.is_empty() {
            "Nothing here yet".to_string()
        } else {
            p.empty.clone()
        };
        buf.set_line(
            x,
            list_y,
            &text::truncate_line(Line::from(span(msg, t.dim())), iw),
            iw as u16,
        );
        return;
    }
    let sel_row = rows
        .iter()
        .position(|r| r.0 == Some(p.selected))
        .unwrap_or(0);
    let first = if sel_row >= list_rows {
        sel_row + 1 - list_rows
    } else {
        0
    };
    // Keep a group header visible above the first item when possible.
    let first = if first > 0 && rows[first - 1].0.is_none() && sel_row - (first - 1) < list_rows {
        first - 1
    } else {
        first
    };
    // Labels line up in a column (short lists: settings, presets).
    let label_w = vis
        .iter()
        .map(|i| text::width(&p.items[*i].label))
        .max()
        .unwrap_or(8)
        .clamp(8, 28);
    for (ri, (vi, group)) in rows.iter().enumerate().skip(first).take(list_rows) {
        let y = list_y + (ri - first) as u16;
        match vi {
            None => {
                buf.set_line(
                    x,
                    y,
                    &Line::from(span(group.to_uppercase(), t.faint())),
                    iw as u16,
                );
            }
            Some(vi) => {
                let it = &p.items[vis[*vi]];
                let selected = *vi == p.selected;
                let base = if selected { t.selected() } else { Style::new() };
                if selected {
                    buf.set_style(
                        Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
                        base,
                    );
                }
                let mark = if selected {
                    span("› ", t.accent())
                } else if it.current {
                    span("● ", t.accent())
                } else {
                    span("  ", Style::new())
                };
                let label_style = if it.disabled {
                    t.faint()
                } else if selected {
                    t.accent_text().add_modifier(Modifier::BOLD)
                } else if it.current {
                    t.accent_text()
                } else {
                    t.text()
                };
                let right: Vec<Span<'static>> = if p.kind == PickKind::Theme {
                    match super::theme::ThemePref::parse(&it.value) {
                        Some(pref) if t.has_color() => t.with_pref(pref).swatch(),
                        _ => Vec::new(),
                    }
                } else if it.badge.is_empty() {
                    Vec::new()
                } else {
                    vec![span(
                        it.badge.clone(),
                        if it.disabled { t.warning() } else { t.faint() },
                    )]
                };
                let badge_w = if right.is_empty() {
                    0
                } else {
                    text::line_width(&Line::from(right.clone())) + 2
                };
                let short = !filter && p.kind != PickKind::Chats;
                let label = text::truncate(
                    &text::sanitize(&it.label),
                    if short {
                        label_w
                    } else {
                        iw.saturating_sub(badge_w + 4).clamp(10, 56)
                    },
                );
                let mut spans = vec![mark, span(label.clone(), label_style)];
                let mut used = 2 + text::width(&label);
                if short && used < label_w + 2 {
                    spans.push(span(" ".repeat(label_w + 2 - used), Style::new()));
                    used = label_w + 2;
                }
                let detail_room = iw.saturating_sub(used + badge_w + 2);
                if !it.detail.is_empty() && detail_room > 4 {
                    spans.push(span(
                        format!(
                            "  {}",
                            text::truncate(&it.detail, detail_room.saturating_sub(2))
                        ),
                        if selected { t.muted() } else { t.dim() },
                    ));
                }
                let line_w: usize = spans.iter().map(|s| text::width(&s.content)).sum();
                if !right.is_empty() {
                    spans.push(span(
                        " ".repeat(iw.saturating_sub(line_w + badge_w - 2).max(1)),
                        Style::new(),
                    ));
                    spans.extend(right);
                }
                let line = text::truncate_line(Line::from(spans), iw).patch_style(base);
                buf.set_line(x, y, &line, iw as u16);
                hits.items
                    .push((Rect::new(inner.x, y, inner.width, 1), *vi));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tui::app::tests::test_app;
    use crate::tui::app::{Info, PickItem};
    use crate::tui::pane::tests::info;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    /// The screen as text, without the padding cell after wide glyphs.
    pub(crate) fn screen_of(term: &Terminal<TestBackend>) -> String {
        let w = term.backend().buffer().area.width as usize;
        let mut out = String::new();
        for row in term.backend().buffer().content().chunks(w.max(1)) {
            let mut skip = 0;
            let mut line = String::new();
            for c in row {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                line.push_str(c.symbol());
                skip = text::width(c.symbol()).saturating_sub(1);
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    pub(crate) fn shot(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        screen_of(&term)
    }

    fn session(id: &str) -> SessionInfo {
        let mut s = info(id, SessionStatus::Idle);
        s.title = "Fix the failing test".into();
        s.cwd = "/home/user/projects/example".into();
        s.model = ModelRef {
            provider: "local".into(),
            model: "qwen3.8-27b".into(),
        };
        s.context = ContextStats {
            tokens: 42_000,
            window: 232_448,
            compactions: 0,
            source: Some("estimated".into()),
        };
        s.lean = true;
        s
    }

    fn fill(p: &mut Pane) {
        let model = ModelRef {
            provider: "local".into(),
            model: "qwen".into(),
        };
        let asst = |content| EntryBody::Assistant {
            content,
            model: model.clone(),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
            error: None,
            author: None,
        };
        let entries = vec![
            EntryBody::User {
                content: vec![ContentPart::text("Run the tests and fix what fails")],
                author: None,
            },
            asst(vec![
                AssistantPart::Thinking {
                    text: "Look at the tests first".into(),
                    opaque: None,
                },
                AssistantPart::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "cargo test -p parser"}),
                },
            ]),
            EntryBody::ToolResult {
                call_id: "c1".into(),
                tool: "bash".into(),
                content: vec![ContentPart::text(
                    "test parse_dates ... FAILED\n\nexit code 101",
                )],
                is_error: true,
                artifact: None,
                details: None,
            },
            asst(vec![AssistantPart::ToolCall {
                id: "c2".into(),
                name: "edit".into(),
                arguments: json!({"path": "/home/user/projects/example/src/dates.rs"}),
            }]),
            EntryBody::ToolResult {
                call_id: "c2".into(),
                tool: "edit".into(),
                content: vec![ContentPart::text("ok")],
                is_error: false,
                artifact: None,
                details: Some(json!({"diff": "--- a/src/dates.rs\n+++ b/src/dates.rs\n@@ -12,1 +12,1 @@\n-    let y = s[0..2].parse()?;\n+    let y = s[0..4].parse()?;\n"})),
            },
            asst(vec![AssistantPart::Text {
                text: "The year was read from **two** digits. Fixed in `src/dates.rs`; all 12 tests pass now.".into(),
            }]),
        ];
        for (i, body) in entries.into_iter().enumerate() {
            p.transcript.add_entry(Entry {
                seq: i as u64 + 1,
                ts: 0,
                body,
            });
        }
    }

    const SIZES: [(u16, u16); 12] = [
        (0, 0),
        (1, 1),
        (10, 3),
        (19, 5),
        (20, 6),
        (30, 8),
        (44, 12),
        (60, 16),
        (80, 24),
        (120, 40),
        (200, 60),
        (250, 70),
    ];

    #[tokio::test(flavor = "multi_thread")]
    async fn draws_at_any_size_with_one_or_more_chats() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(session("s1"));
        fill(app.pane_mut());
        app.pane_mut().editor.set_text("and now the docs");
        for (w, h) in SIZES {
            app.screen = Rect::new(0, 0, w, h);
            let _ = shot(&mut app, w, h);
        }
        app.screen = Rect::new(0, 0, 200, 50);
        app.run_command("/split");
        for (w, h) in SIZES {
            app.screen = Rect::new(0, 0, w, h);
            let _ = shot(&mut app, w, h);
        }
        // Popups at every size too.
        app.run_command("/settings");
        for (w, h) in SIZES {
            let _ = shot(&mut app, w, h);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_chat_fills_the_screen_with_a_status_line() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(session("s1"));
        fill(app.pane_mut());
        let s = shot(&mut app, 100, 30);
        let rows: Vec<&str> = s.lines().collect();
        // The conversation, the message box and the status line under it.
        assert!(s.contains("▎ Run the tests and fix what fails"), "{s}");
        assert!(s.contains("Fixed in src/dates.rs"), "{s}");
        assert!(rows[rows.len() - 4].trim_start().starts_with("╭"), "{s}");
        assert!(rows[rows.len() - 3].contains("Ask anything"), "{s}");
        let status = rows[rows.len() - 1];
        for want in [
            "ready",
            "qwen3.8-27b",
            "/home/user/projects/example",
            "42k/232k",
        ] {
            assert!(status.contains(want), "{want} in {status:?}");
        }
        // No tiles, no top bar, no mosaic.
        assert!(!s.contains("Main") && !s.contains("mosaic"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_chat_says_what_it_is_and_the_keys() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(session("s1"));
        let s = shot(&mut app, 90, 24);
        assert!(
            s.contains("Tessera  a coding agent in your terminal"),
            "{s}"
        );
        assert!(
            s.contains("qwen3.8-27b  ·  /home/user/projects/example"),
            "{s}"
        );
        assert!(s.contains("/ commands") && s.contains("? help"), "{s}");
        // Said once: the message box doesn't repeat the welcome's keys.
        assert_eq!(s.matches("/ commands").count(), 1, "{s}");
        // Without a model: what to do.
        let (mut app2, dir2) = test_app().await;
        app2.boot = Boot::NoModel;
        let s = shot(&mut app2, 90, 24);
        assert!(s.contains("No model is set up yet."), "{s}");
        assert!(s.contains("tessera setup"), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approvals_replace_the_message_box() {
        let (mut app, dir) = test_app().await;
        let mut s = session("s1");
        s.status = SessionStatus::AwaitingApproval;
        app.pane_mut().apply_info(s);
        app.pane_mut().approvals.push(ApprovalRequest {
            id: "r1".into(),
            call_id: "c".into(),
            tool: "bash".into(),
            summary: "rm -rf target/".into(),
            reason: "Deletes files".into(),
            arguments: serde_json::Value::Null,
            once: false,
        });
        let s = shot(&mut app, 90, 24);
        assert!(s.contains("Approval needed"), "{s}");
        assert!(s.contains("bash  rm -rf target/"), "{s}");
        assert!(
            s.contains("y allow   a allow for this chat   n deny"),
            "{s}"
        );
        assert!(s.contains("needs your approval"), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_long_approval_scrolls_and_is_allowed_only_once_read() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, dir) = test_app().await;
        let mut s = session("s1");
        s.status = SessionStatus::AwaitingApproval;
        app.pane_mut().apply_info(s);
        // Shown with its line ends spelled out, so it wraps as one long text.
        let typed: Vec<String> = (1..=200).map(|i| format!("line {i} of the text")).collect();
        app.pane_mut().approvals.push(ApprovalRequest {
            id: "r1".into(),
            call_id: "c".into(),
            tool: "desktop".into(),
            summary: "type 1234 characters".into(),
            reason: typed.join("\n"),
            arguments: serde_json::Value::Null,
            once: true,
        });
        app.pane_mut().approval_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        let s = shot(&mut app, 90, 24);
        // The keys stay; what is below is said; no "for this chat" when
        // the machine asks every time.
        assert!(s.contains("more lines to read"), "{s}");
        assert!(s.contains("y allow   n deny"), "{s}");
        assert!(!s.contains("allow for this chat"), "{s}");
        assert!(!s.contains("line 200 of the text"), "{s}");
        // Not allowed unseen ("a" does nothing here either).
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.pane().approvals.len(), 1);
        // Jumping to the end shows the end, but what was skipped is unread.
        app.on_key(KeyEvent::from(KeyCode::End));
        let s = shot(&mut app, 90, 24);
        assert!(s.contains("line 200 of the text"), "{s}");
        assert!(s.contains("more lines to read"), "{s}");
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.pane().approvals.len(), 1);
        // Read through from the top (a line, then pages), it can be allowed.
        app.on_key(KeyEvent::from(KeyCode::Home));
        shot(&mut app, 90, 24);
        app.on_key(KeyEvent::from(KeyCode::Down));
        shot(&mut app, 90, 24);
        assert_eq!(app.pane().approval_view.scroll, 1);
        for _ in 0..40 {
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
            shot(&mut app, 90, 24);
        }
        let s = shot(&mut app, 90, 24);
        assert!(
            s.contains("line 200 of the text") && s.contains("↑ back up"),
            "{s}"
        );
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(app.pane().approvals.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_approval_without_room_for_its_text_cannot_be_allowed() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, dir) = test_app().await;
        let mut s = session("s1");
        s.status = SessionStatus::AwaitingApproval;
        app.pane_mut().apply_info(s);
        app.pane_mut().approvals.push(ApprovalRequest {
            id: "r1".into(),
            call_id: "c".into(),
            tool: "desktop".into(),
            summary: "type 5 characters".into(),
            reason: "Uses the mouse or keyboard: type “hello”.".into(),
            arguments: serde_json::Value::Null,
            once: true,
        });
        app.pane_mut().approval_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        // A terminal this short has no room for the text: nothing was seen.
        shot(&mut app, 90, 6);
        app.on_key(KeyEvent::from(KeyCode::End));
        shot(&mut app, 90, 6);
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.pane().approvals.len(), 1);
        // Read in a narrow terminal, then widened: its lines are others, so
        // reading starts again (a short one fits at once and is read).
        shot(&mut app, 50, 24);
        assert!(app.pane().approval_view.read);
        app.on_key(KeyEvent::from(KeyCode::End));
        shot(&mut app, 90, 6);
        assert!(!app.pane().approval_view.read);
        // Taller, it shows it and can be allowed.
        shot(&mut app, 90, 24);
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(app.pane().approvals.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chats_side_by_side_show_their_titles() {
        let (mut app, dir) = test_app().await;
        app.screen = Rect::new(0, 0, 160, 30);
        app.pane_mut().apply_info(session("s1"));
        app.run_command("/split");
        let mut second = session("s2");
        second.title = "Write the docs".into();
        app.pane_mut().apply_info(second);
        let s = shot(&mut app, 160, 30);
        let first = s.lines().next().unwrap();
        assert!(first.contains("○ Fix the failing test"), "{first}");
        assert!(first.contains("● Write the docs"), "{first}");
        assert!(s.lines().nth(5).unwrap().contains('│'), "{s}");
        // Narrowed until only one fits: the focused chat, saying the other
        // is a keypress away; alt+← brings it.
        let s = shot(&mut app, 70, 24);
        let first = s.lines().next().unwrap();
        assert!(first.contains("● Write the docs"), "{s}");
        assert!(first.ends_with("1 more alt+←/→"), "{first}");
        assert!(!s.contains("Fix the failing test"), "{s}");
        assert!(!s.lines().nth(5).unwrap().contains('│'), "{s}");
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::ALT,
        ));
        let s = shot(&mut app, 70, 24);
        assert!(
            s.lines().next().unwrap().contains("● Fix the failing test"),
            "{s}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn popups_list_and_help() {
        let (mut app, dir) = test_app().await;
        app.pane_mut().apply_info(session("s1"));
        app.run_command("/settings");
        let s = shot(&mut app, 100, 30);
        for want in ["Settings", "› Permissions", "Model for new chats", "Mouse"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        // On the terminal's own colours, a popup's title is the text's
        // colour, not the border's.
        app.theme = Theme::new(
            super::super::theme::Depth::True,
            super::super::theme::ThemePref::Auto,
            Some(super::super::theme::Tone::Dark),
        );
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let at = screen_of(&term)
            .lines()
            .enumerate()
            .find_map(|(y, l)| l.find(" Settings ").map(|x| (l[..x].chars().count(), y)))
            .expect("the title");
        let cell = &buf[(at.0 as u16 + 1, at.1 as u16)];
        assert_eq!(cell.symbol(), "S");
        assert_eq!(cell.fg, ratatui::style::Color::Reset);
        app.theme = Theme::test();
        app.modal = Some(Modal::Info(Info {
            title: "Help".into(),
            lines: vec![Line::from("x")],
            scroll: 0,
        }));
        let s = shot(&mut app, 100, 30);
        assert!(s.contains("Help") && s.contains("esc close"), "{s}");
        let mut picker = Picker::new(PickKind::Chats, "Your chats");
        picker.loading = false;
        picker
            .items
            .push(PickItem::new("Fix the failing test", "s1").detail("just now"));
        app.modal = Some(Modal::Picker(picker));
        let s = shot(&mut app, 100, 30);
        assert!(
            s.contains("Type to search") && s.contains("› Fix the failing test"),
            "{s}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn plain_and_no_color_render_without_escapes() {
        let (mut app, dir) = test_app().await;
        app.theme = Theme::new(super::super::theme::Depth::None, Default::default(), None);
        app.pane_mut().apply_info(session("s1"));
        fill(app.pane_mut());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        for c in term.backend().buffer().content() {
            assert!(c.fg == ratatui::style::Color::Reset && c.bg == ratatui::style::Color::Reset);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stable_split_respects_fences() {
        assert_eq!(stable_split("a\n\nb"), 3);
        assert_eq!(stable_split("```\na\n\nb\n```"), 0);
        assert_eq!(stable_split("one"), 0);
    }
}
