//! `whiteboard`: show the user something in a tile beside the chat, ask
//! them for input there, or look at what a board shows (including pages the
//! user opened themselves).

use base64::Engine as _;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{AskAnswer, AskInput, BoardContent, ContentPart};

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, opt_str, opt_u64};

pub struct Whiteboard;

const MAX_IMAGE_FILE: u64 = 10 << 20;
/// Text of a board shown at once; `offset` pages through the rest.
const READ_CHARS: usize = 20_000;
/// Source excerpt shown with an HTML board's text.
const SOURCE_CHARS: usize = 3_000;
/// Larger images are described, not sent to the model.
const MAX_INLINE_IMAGE: usize = 5 << 20;

/// A `Local` board's page as its tile gets it: status, content type, body.
pub type LocalPage = (u16, String, Vec<u8>);

/// A whiteboard as an agent sees it.
pub struct BoardView {
    pub board: tessera_proto::Whiteboard,
    /// The chat it belongs to, when that isn't this conversation.
    pub other_chat: Option<String>,
    /// Where its tiles are, e.g. `in "Work" beside this chat`.
    pub on_screen: Vec<String>,
}

impl Tool for Whiteboard {
    fn name(&self) -> &'static str {
        "whiteboard"
    }
    fn description(&self) -> String {
        "Visuals the user asked for. `show` one of: `html` (self-contained, no network; see `docs whiteboards`), \
         `markdown`, `image_path`, `url`, `local_port` (a web app here): a card in the chat; `open: true` also \
         opens a tile, only if asked. `id` updates it. `ask` waits for a `signature` (PNG), `text`, `choice` or \
         `confirm`. `read`/`look` (`id`/`title`, else latest): its content, or the user's screen of it (also \
         pages they opened); `list` shows all."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["show", "ask", "read", "look", "list", "delete"]},
                "id": {"type": "string"},
                "title": {"type": "string"},
                "html": {"type": "string"},
                "markdown": {"type": "string"},
                "image_path": {"type": "string"},
                "url": {"type": "string"},
                "local_port": {"type": "integer"},
                "local_path": {"type": "string"},
                "prompt": {"type": "string"},
                "input": {"type": "string", "enum": ["signature", "text", "choice", "confirm"]},
                "options": {"type": "array", "items": {"type": "string"}},
                "multiline": {"type": "boolean"},
                "open": {"type": "boolean"},
                "offset": {"type": "integer"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, args: &Value) -> bool {
        // A read checks the page it fetches is the one cleared (`ctx.cleared`),
        // so only asks wait their turn; a `look` never holds others up.
        opt_str(args, "action") != Some("ask")
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("show");
        match opt_str(args, "title") {
            Some(t) => format!("{action} · {t}"),
            None => action.to_string(),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        if opt_str(args, "action") == Some("read") {
            // The server fetches a web page board as `web_fetch` would.
            return match target(args, ctx).map(|v| v.board.content) {
                Ok(BoardContent::Url { url }) => super::web::fetch_risk(&url),
                _ => Risk::None,
            };
        }
        match opt_str(args, "image_path") {
            Some(p) => super::path_risk(&ctx.resolve(p), ctx),
            None => Risk::None,
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let id = opt_str(&args, "id").map(str::to_string);
            let title = opt_str(&args, "title").map(str::to_string);
            match opt_str(&args, "action").unwrap_or("show") {
                "list" => {
                    let boards = ctx.host.board_list(&ctx.session_id);
                    if boards.is_empty() {
                        return ToolOutput::text("No whiteboards in this conversation.");
                    }
                    let lines: Vec<String> = boards.iter().map(describe).collect();
                    let mut out = lines
                        .iter()
                        .take(40)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    if lines.len() > 40 {
                        out.push_str(&format!("\n…and {} more", lines.len() - 40));
                    }
                    ToolOutput::text(out)
                }
                "read" => match target(&args, ctx) {
                    Ok(view) => read(view, &args, ctx).await,
                    Err(e) => ToolOutput::error(e),
                },
                "look" => look(&args, ctx).await,
                "delete" => {
                    let Some(id) = id else {
                        return ToolOutput::error("`id` is required.");
                    };
                    // A web view open on a machine: closing it there is an
                    // action there, asked of that machine (on the run it allows).
                    let admitted = match ctx.host.browser() {
                        Some(b) => match b.admit_board(&id, &ctx.session_id, "close").await {
                            Ok(a) => a,
                            Err(e) => return ToolOutput::error(e),
                        },
                        None => None,
                    };
                    let deleted = crate::browser::on_run(admitted, async {
                        ctx.host.board_delete(&ctx.session_id, &id)
                    })
                    .await;
                    match deleted {
                        Ok(()) => ToolOutput::text(format!("Deleted whiteboard {id}.")),
                        Err(e) => ToolOutput::error(e),
                    }
                }
                "ask" => ask(&args, id, title, ctx).await,
                "show" => {
                    let content = match content(&args, ctx).await {
                        Ok(c) => c,
                        Err(e) => return ToolOutput::error(e),
                    };
                    let tile = open(&args);
                    // A web page on a machine: its tile (new, or one already on
                    // screen) loads it there, so asked first whatever `open`.
                    let asked = match ask_machine_first(ctx, id.as_deref(), Some(&content)).await {
                        Ok(m) => m,
                        Err(e) => return ToolOutput::error(e),
                    };
                    // A new page goes where that was decided (the machine
                    // asked, or this server), never where it would go later.
                    let host = (id.is_none() && matches!(content, BoardContent::Url { .. }))
                        .then_some(asked);
                    match ctx.host.board_show_on(
                        &ctx.session_id,
                        id.as_deref(),
                        title.as_deref(),
                        content,
                        tile,
                        host,
                    ) {
                        Ok(b) => {
                            let on_screen = tile
                                || ctx
                                    .host
                                    .board_list(&ctx.session_id)
                                    .iter()
                                    .any(|v| v.board.id == b.id && !v.on_screen.is_empty());
                            ToolOutput::text(format!(
                                "{} whiteboard {} (\"{}\"); {}. Pass `id: {}` to update it.",
                                if id.is_some() { "Updated" } else { "Made" },
                                b.id,
                                b.title,
                                if on_screen {
                                    "it is in a tile beside this chat"
                                } else {
                                    "it is a card in the chat that the user can open"
                                },
                                b.id
                            ))
                            .with_details(json!({"board": {"id": b.id, "title": b.title, "kind": kind(&b.content)}}))
                        }
                        Err(e) => ToolOutput::error(e),
                    }
                }
                other => ToolOutput::error(format!("Unknown action `{other}`.")),
            }
        })
    }
}

/// Whether the agent was asked to put the board on screen (`open: true`;
/// `tile`/`show` say the same).
/// Putting a web view on screen when its page runs on a machine (an
/// existing board's, or where a new page would go) makes the user's app load
/// it there: an action there, asked of that machine first with this chat's
/// turn's count of Stops (taken now).
/// The machine it was asked of (`None`: none, e.g. this server's page).
async fn ask_machine_first(
    ctx: &ToolCtx,
    id: Option<&str>,
    content: Option<&BoardContent>,
) -> Result<Option<String>, String> {
    let Some(browsers) = ctx.host.browser() else {
        return Ok(None);
    };
    let machine = match id {
        // What it will be (its new content, if any) on the host it keeps.
        Some(id) => ctx
            .host
            .board_list(&ctx.session_id)
            .into_iter()
            .find(|v| v.board.id == id)
            .filter(|v| {
                matches!(
                    content.unwrap_or(&v.board.content),
                    BoardContent::Url { .. }
                )
            })
            .and_then(|v| v.board.host),
        None => match content {
            Some(BoardContent::Url { .. }) => browsers.place(&ctx.session_id, None).ok().flatten(),
            _ => None,
        },
    };
    let Some(machine) = machine else {
        return Ok(None);
    };
    let mark = browsers.turn_mark_on(Some(&machine), &ctx.session_id);
    if ctx.cancel.is_cancelled() {
        return Err("Stopped.".into());
    }
    browsers
        .admit_on(Some(&machine), &ctx.session_id, mark, "show", "")
        .await?;
    Ok(Some(machine))
}

fn open(args: &Value) -> bool {
    ["open", "tile", "show"]
        .iter()
        .any(|k| args.get(*k).and_then(Value::as_bool) == Some(true))
}

/// One line about a board: id, title, kind, who opened it, where it is.
fn describe(v: &BoardView) -> String {
    let b = &v.board;
    let mut s = format!("{} · \"{}\" · {}", b.id, b.title, kind(&b.content));
    if b.by_user {
        s.push_str(" · opened by the user");
    }
    if let Some(chat) = &v.other_chat {
        s.push_str(&format!(" · from the chat \"{chat}\""));
    }
    if v.on_screen.is_empty() {
        s.push_str(" · not on screen");
    } else {
        s.push_str(&format!(" · on screen {}", v.on_screen.join(", ")));
    }
    if b.ask.is_some() {
        s.push_str(" · waiting for an answer");
    }
    s
}

/// The board `id` or `title` names; else the latest on screen (or at all).
fn target(args: &Value, ctx: &ToolCtx) -> Result<BoardView, String> {
    let mut views = ctx.host.board_list(&ctx.session_id);
    let pos = match (opt_str(args, "id"), opt_str(args, "title")) {
        (Some(id), _) => views.iter().position(|v| v.board.id == id.trim()),
        (None, Some(t)) => {
            let t = t.trim().to_lowercase();
            let title = |v: &BoardView| v.board.title.to_lowercase();
            views
                .iter()
                .position(|v| title(v) == t)
                .or_else(|| views.iter().position(|v| title(v).contains(&t)))
        }
        (None, None) => views
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| (!v.on_screen.is_empty(), v.board.updated_at))
            .map(|(i, _)| i),
    };
    match pos {
        Some(i) => Ok(views.swap_remove(i)),
        None if views.is_empty() => Err("No whiteboards in this conversation.".into()),
        None => Err("No such whiteboard; `list` shows them.".into()),
    }
}

/// What a board shows, as the model can take it: text (capped, pageable
/// with `offset`), or the image itself for a model with vision.
/// What the user's device shows of a board's tile, for a model that sees
/// images; otherwise (or when no device can show it) its content, and why.
async fn look(args: &Value, ctx: &ToolCtx) -> ToolOutput {
    let view = match target(args, ctx) {
        Ok(v) => v,
        Err(e) => return ToolOutput::error(e),
    };
    let why = if !ctx.host.sees_images(&ctx.session_id) {
        "this model can't see images".to_string()
    } else {
        let id = view.board.id.clone();
        match ctx
            .host
            .board_look(&ctx.session_id, &id, ctx.cancel.clone())
            .await
        {
            Ok(img) => {
                return ToolOutput {
                    content: vec![
                        ContentPart::text(format!(
                            "{}\nWhat the user's screen shows of it ({}×{}):",
                            describe(&view),
                            img.width,
                            img.height
                        )),
                        ContentPart::Image {
                            mime: img.mime,
                            data: img.data,
                        },
                    ],
                    is_error: false,
                    details: None,
                };
            }
            Err(e) => e,
        }
    };
    let mut out = read(view, args, ctx).await;
    if let Some(ContentPart::Text { text }) = out.content.first_mut() {
        *text = format!("[Couldn't see the tile ({why}); its content instead.]\n{text}");
    }
    out
}

async fn read(view: BoardView, args: &Value, ctx: &ToolCtx) -> ToolOutput {
    let others = ctx.host.board_list(&ctx.session_id).len().saturating_sub(1);
    let mut head = describe(&view);
    if others > 0 && opt_str(args, "id").is_none() {
        head.push_str(&format!(" ({others} other boards: `list`)"));
    }
    let b = view.board;
    let text = match b.content {
        BoardContent::Markdown { markdown } => crate::scan::strip_hidden(&markdown).0,
        BoardContent::Html { html } => {
            let text = super::web::readable(html.as_bytes(), "text/html", "");
            let (source, _) = crate::scan::strip_hidden(&html);
            let cut = crate::util::floor_char(&source, SOURCE_CHARS.min(source.len()));
            let more = if cut < source.len() { "\n…" } else { "" };
            format!(
                "{text}\n\nSource (HTML, first {cut} of {} characters):\n```html\n{}{more}\n```",
                source.len(),
                &source[..cut]
            )
        }
        BoardContent::Image { mime, data } => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_bytes())
                .unwrap_or_default();
            let facts = image_facts(&mime, &bytes);
            if ctx.host.sees_images(&ctx.session_id) && bytes.len() <= MAX_INLINE_IMAGE {
                return ToolOutput {
                    content: vec![
                        ContentPart::text(format!("{head}\n{facts}:")),
                        ContentPart::Image { mime, data },
                    ],
                    is_error: false,
                    details: None,
                };
            }
            let why = if bytes.len() > MAX_INLINE_IMAGE {
                "too large to send you"
            } else {
                "you can't see images with this model"
            };
            let mut out = format!("{head}\n{facts} ({why}).");
            if mime == "image/svg+xml" {
                let svg = crate::scan::strip_hidden(&String::from_utf8_lossy(&bytes)).0;
                let cut = crate::util::floor_char(&svg, SOURCE_CHARS.min(svg.len()));
                out.push_str(&format!("\nSVG source:\n```svg\n{}\n```", &svg[..cut]));
            }
            return ToolOutput::text(out);
        }
        BoardContent::Url { url } => {
            let parsed = match url::Url::parse(&url) {
                Ok(u) => u,
                Err(_) => return ToolOutput::error(format!("`{url}` isn't a URL.")),
            };
            // Only the page that was cleared: an internal address needs
            // the same approval `web_fetch` asks for (`look` asks for none).
            if let Risk::Ask { key, .. } = super::web::fetch_risk(&url)
                && ctx.cleared.as_deref() != Some(key.as_str())
            {
                return ToolOutput::error(
                    "This page is on a local network address: `read` it (the user is asked first).",
                );
            }
            match super::web::fetch_page(ctx, parsed).await {
                Ok(p) => format!("{} (HTTP {})\n\n{}", p.url, p.status, p.text),
                Err(e) => return ToolOutput::error(e),
            }
        }
        BoardContent::Local { port, path } => {
            match ctx.host.board_local_get(&ctx.session_id, &b.id).await {
                Ok((status, ctype, body)) => format!(
                    "http://127.0.0.1:{port}{path} (HTTP {status})\n\n{}",
                    super::web::readable(&body, &ctype, "")
                ),
                Err(e) => return ToolOutput::error(e),
            }
        }
    };
    let offset = opt_u64(args, "offset").unwrap_or(0) as usize;
    let start = crate::util::ceil_char(&text, offset.min(text.len()));
    let end = crate::util::floor_char(&text, (start + READ_CHARS).min(text.len()));
    let mut out = format!("{head}\n\n{}", &text[start..end]);
    if end < text.len() {
        out.push_str(&format!(
            "\n\n[… {} more characters. Continue with offset={end}]",
            text.len() - end
        ));
    }
    ToolOutput::text(out)
}

/// "PNG image, 1280×720, 245 KB" from the header bytes (no decoding).
fn image_facts(mime: &str, b: &[u8]) -> String {
    let kind = mime
        .trim_start_matches("image/")
        .to_uppercase()
        .replace("+XML", "");
    match image_size(mime, b) {
        Some((w, h)) => format!("{kind} image, {w}×{h}, {} KB", b.len().div_ceil(1024)),
        None => format!("{kind} image, {} KB", b.len().div_ceil(1024)),
    }
}

/// An image's width and height from its header bytes (PNG, GIF, JPEG, WebP).
pub(crate) fn image_size(mime: &str, b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as u32)
    };
    let le16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as u32)
    };
    let le24 = |i: usize| {
        b.get(i..i + 3)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]))
    };
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    match mime {
        "image/png" => be32(16).zip(be32(20)),
        "image/gif" => le16(6).zip(le16(8)),
        "image/jpeg" => {
            // The first start-of-frame segment holds the size.
            let mut i = 2;
            while i + 9 < b.len() && b[i] == 0xFF {
                let marker = b[i + 1];
                if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                    return be16(i + 7).zip(be16(i + 5));
                }
                i += 2 + be16(i + 2).unwrap_or(0) as usize;
            }
            None
        }
        // RIFF....WEBP, then a lossy (VP8), lossless (VP8L) or extended (VP8X) chunk.
        "image/webp" => match b.get(12..16)? {
            b"VP8 " => le16(26)
                .zip(le16(28))
                .map(|(w, h)| (w & 0x3FFF, h & 0x3FFF)),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => le24(24).zip(le24(27)).map(|(w, h)| (w + 1, h + 1)),
            _ => None,
        },
        _ => None,
    }
}

fn kind(c: &BoardContent) -> &'static str {
    match c {
        BoardContent::Html { .. } => "html",
        BoardContent::Markdown { .. } => "markdown",
        BoardContent::Image { .. } => "image",
        BoardContent::Url { .. } => "url",
        BoardContent::Local { .. } => "local",
    }
}

/// Exactly one kind of content from the arguments.
async fn content(args: &Value, ctx: &ToolCtx) -> Result<BoardContent, String> {
    let given: Vec<&str> = ["html", "markdown", "image_path", "url", "local_port"]
        .into_iter()
        .filter(|k| args.get(*k).is_some_and(|v| !v.is_null()))
        .collect();
    if given.len() != 1 {
        return Err(
            "Give exactly one of `html`, `markdown`, `image_path`, `url` or `local_port`.".into(),
        );
    }
    Ok(match given[0] {
        "html" => BoardContent::Html {
            html: opt_str(args, "html").unwrap_or_default().to_string(),
        },
        "markdown" => BoardContent::Markdown {
            markdown: opt_str(args, "markdown").unwrap_or_default().to_string(),
        },
        "url" => BoardContent::Url {
            url: opt_str(args, "url").unwrap_or_default().to_string(),
        },
        "local_port" => BoardContent::Local {
            port: args
                .get("local_port")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .ok_or("`local_port` must be a port number")?,
            path: opt_str(args, "local_path").unwrap_or("/").to_string(),
        },
        _ => {
            let path = ctx.resolve(opt_str(args, "image_path").unwrap_or_default());
            let mime = match path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("gif") => "image/gif",
                Some("webp") => "image/webp",
                Some("svg") => "image/svg+xml",
                _ => return Err("`image_path` must be a PNG, JPEG, GIF, WebP or SVG file".into()),
            };
            let bytes = super::files::authorized_bytes(&path, ctx, MAX_IMAGE_FILE).await?;
            BoardContent::Image {
                mime: mime.into(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }
    })
}

async fn ask(args: &Value, id: Option<String>, title: Option<String>, ctx: &ToolCtx) -> ToolOutput {
    let Some(prompt) = opt_str(args, "prompt").map(str::to_string) else {
        return ToolOutput::error("`prompt` is required.");
    };
    let input = match opt_str(args, "input").unwrap_or("confirm") {
        "signature" => AskInput::Signature,
        "text" => AskInput::Text {
            multiline: args
                .get("multiline")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            placeholder: String::new(),
        },
        "choice" => AskInput::Choice {
            options: args
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            multi: false,
        },
        "confirm" => AskInput::Confirm,
        other => return ToolOutput::error(format!("Unknown input `{other}`.")),
    };
    let options = match &input {
        AskInput::Choice { options, .. } => options.clone(),
        _ => Vec::new(),
    };
    (ctx.progress)("Waiting for the user's answer…\n");
    // The question shows in the chat; a board it is about goes on screen.
    let tile = id.is_some() || open(args);
    if tile
        && let Some(board) = id.as_deref()
        && let Err(e) = ask_machine_first(ctx, Some(board), None).await
    {
        return ToolOutput::error(e);
    }
    let answer = ctx
        .host
        .board_ask(
            &ctx.session_id,
            id,
            title,
            prompt,
            input,
            tile,
            ctx.cancel.clone(),
        )
        .await;
    match answer {
        Err(e) => ToolOutput::error(format!("No answer: {e}.")),
        Ok(AskAnswer::Cancel) => ToolOutput::text("The user declined to answer."),
        Ok(AskAnswer::Text { text }) => ToolOutput::text(format!("The user answered:\n{text}")),
        Ok(AskAnswer::Choice { index }) => ToolOutput::text(format!(
            "The user chose: {}",
            options
                .get(index as usize)
                .map(String::as_str)
                .unwrap_or("?")
        )),
        Ok(AskAnswer::Choices { indexes }) => ToolOutput::text(format!(
            "The user chose: {}",
            indexes
                .iter()
                .filter_map(|i| options.get(*i as usize).map(String::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Ok(AskAnswer::Confirm { yes }) => ToolOutput::text(if yes {
            "The user said yes."
        } else {
            "The user said no."
        }),
        Ok(AskAnswer::Signature { png }) => {
            match ctx.host.board_save_signature(&ctx.session_id, &png) {
                Ok(path) => ToolOutput::text(format!(
                    "The user signed. Their signature is saved as {} (PNG with a transparent background). Use it only for \
                 what you asked it for.",
                    path.display()
                )),
                Err(e) => ToolOutput::error(format!("The user signed, but saving it failed: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::image_facts;

    #[test]
    fn image_sizes_come_from_their_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(720u32.to_be_bytes());
        assert_eq!(image_facts("image/png", &png), "PNG image, 1280×720, 1 KB");
        let gif = b"GIF89a\x40\x01\xf0\x00".to_vec();
        assert!(image_facts("image/gif", &gif).contains("320×240"));
        // JPEG: an APP0 segment, then the frame header (SOF0).
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0];
        jpg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x58, 0x03, 0x20, 0, 0]);
        assert!(
            image_facts("image/jpeg", &jpg).contains("800×600"),
            "{}",
            image_facts("image/jpeg", &jpg)
        );
        assert_eq!(image_facts("image/svg+xml", b"<svg/>"), "SVG image, 1 KB");
        assert_eq!(image_facts("image/webp", &[0; 3000]), "WEBP image, 3 KB");
    }
}
