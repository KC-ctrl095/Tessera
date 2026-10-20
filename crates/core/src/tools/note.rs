//! `note`: the user's notes library, markdown documents they keep (like
//! artifacts). Whiteboards show something now; notes are kept.

use base64::Engine as _;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::ContentPart;

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, opt_str, opt_u64};

pub struct Note;

/// Text of a note shown at once; `offset` pages through the rest.
const READ_CHARS: usize = 30_000;
/// Pictures sent with a read, and their bytes in all, at most.
const READ_PICTURES: usize = 4;
const READ_PICTURE_BYTES: usize = 8 << 20;

impl Tool for Note {
    fn name(&self) -> &'static str {
        "note"
    }
    fn description(&self) -> String {
        "The user's notes: markdown documents they keep. `list`, `read`, `create` \
         (`title`, `content`), `edit` (exact `find` → `replace`), `update` (`content` replaces: pass the `version` \
         you read; or `append`), `image` (a picture file at `path` added to note `id`, or kept for you to place). \
         `show: true` opens it on screen, only if asked. Notes link to each other with [[Title]]; their pictures are \
         `![alt](assets/<name>)`, keep those as they are. Write one when asked or clearly worth keeping."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "read", "create", "edit", "update", "image"]},
                "id": {"type": "string"},
                "title": {"type": "string"},
                "content": {"type": "string"},
                "append": {"type": "string"},
                "find": {"type": "string"},
                "replace": {"type": "string"},
                "version": {"type": "integer"},
                "show": {"type": "boolean"},
                "offset": {"type": "integer"},
                "path": {"type": "string"},
                "alt": {"type": "string"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn action_kind(&self, args: &Value) -> ToolKind {
        match opt_str(args, "action").unwrap_or("list") {
            "list" | "read" => ToolKind::Read,
            _ => ToolKind::Write,
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        // A picture is read from a file: checked like any file read.
        match (opt_str(args, "action"), opt_str(args, "path")) {
            (Some("image"), Some(p)) => super::path_risk(&ctx.resolve(p), ctx),
            _ => Risk::None,
        }
    }
    fn summary(&self, args: &Value) -> String {
        let action = opt_str(args, "action").unwrap_or("list");
        match opt_str(args, "title").or(opt_str(args, "id")) {
            Some(t) => format!("{action} · {t}"),
            None => action.to_string(),
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(why) = ctx.authorize_mutation(self, &args) {
                return ToolOutput::error(why);
            }
            for key in ["title", "content", "append", "replace", "alt"] {
                let hidden = opt_str(&args, key).map_or(0, |t| crate::scan::strip_hidden(t).1);
                if hidden > 0 {
                    return ToolOutput::error(format!(
                        "`{key}` contains {hidden} hidden Unicode characters; write notes as plain visible text."
                    ));
                }
            }
            let id = opt_str(&args, "id").map(str::trim);
            let out = match opt_str(&args, "action").unwrap_or("list") {
                "list" => return list(ctx),
                "read" => match id.and_then(|id| ctx.host.note(&ctx.session_id, id)) {
                    Some(n) if args.get("show").and_then(Value::as_bool) == Some(true) => {
                        let mut out = read(&n, &args, ctx);
                        if ctx.host.note_show(&ctx.session_id, &n.id).is_ok() {
                            out.content
                                .push(ContentPart::text("It is open beside this chat."));
                            let mut d = out.details.take().unwrap_or_else(|| json!({}));
                            d["note"] = card(&n.id, &n.title, n.version, "read");
                            out.details = Some(d);
                        }
                        return out;
                    }
                    Some(n) => return read(&n, &args, ctx),
                    None => return ToolOutput::error("No such note; `list` shows them."),
                },
                "create" => {
                    let (Some(title), Some(content)) =
                        (opt_str(&args, "title"), opt_str(&args, "content"))
                    else {
                        return ToolOutput::error("`title` and `content` are required.");
                    };
                    ctx.host
                        .note_save(&ctx.session_id, None, title, content.into(), None)
                        .map(|n| {
                            let text =
                                format!("Created note {} \"{}\" (version 1).", n.id, n.title);
                            (text, n.id)
                        })
                }
                "edit" => edit(&args, id, ctx),
                "update" => update(&args, id, ctx),
                "image" => image(&args, id, ctx).await,
                other => return ToolOutput::error(format!("Unknown action `{other}`.")),
            };
            match out {
                Ok((mut text, id)) => {
                    if args.get("show").and_then(Value::as_bool) == Some(true) && !id.is_empty() {
                        match ctx.host.note_show(&ctx.session_id, &id) {
                            Ok(()) => text.push_str(" It is open beside this chat."),
                            Err(e) => text.push_str(&format!(" (Not shown: {e}.)")),
                        }
                    }
                    // The chat's card for it (one tap opens it).
                    match ctx.host.note(&ctx.session_id, &id) {
                        Some(n) => {
                            let action = opt_str(&args, "action").unwrap_or_default();
                            ToolOutput::text(text).with_details(
                                json!({"note": card(&n.id, &n.title, n.version, action)}),
                            )
                        }
                        None => ToolOutput::text(text),
                    }
                }
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

/// What the chat's card of a note the agent wrote shows.
fn card(id: &str, title: &str, version: u64, action: &str) -> Value {
    json!({"id": id, "title": title, "version": version, "action": action})
}

fn list(ctx: &ToolCtx) -> ToolOutput {
    let notes = ctx.host.notes(&ctx.session_id);
    if notes.is_empty() {
        return ToolOutput::text("No notes yet.");
    }
    let mut out: Vec<String> = notes
        .iter()
        .take(50)
        .map(|n| {
            format!(
                "{} · \"{}\" · {} · updated {} · {}",
                n.id,
                n.title,
                if n.by_user {
                    "by the user"
                } else {
                    "by an agent"
                },
                crate::jobs::fmt_time(n.updated_at),
                n.preview
            )
        })
        .collect();
    if notes.len() > 50 {
        out.push(format!("…and {} more", notes.len() - 50));
    }
    ToolOutput::text(out.join("\n"))
}

/// A note as data: hidden characters removed and instruction-like text
/// labelled (the user is told), paged by `offset`. The pictures in the part
/// shown come along when the model takes pictures.
fn read(n: &tessera_proto::Note, args: &Value, ctx: &ToolCtx) -> ToolOutput {
    let scan = crate::scan::scan(&n.content);
    let text = &scan.text;
    let offset = opt_u64(args, "offset").unwrap_or(0) as usize;
    let start = crate::util::ceil_char(text, offset.min(text.len()));
    let end = crate::util::floor_char(text, (start + READ_CHARS).min(text.len()));
    let mut out = format!(
        "Note {} \"{}\" (version {}, {}, updated {})\n{}\n{}",
        n.id,
        n.title,
        n.version,
        if n.by_user {
            "by the user"
        } else {
            "by an agent"
        },
        crate::jobs::fmt_time(n.updated_at),
        if scan.is_clean() {
            String::new()
        } else {
            format!("{}\n", crate::scan::label("note", &scan))
        },
        &text[start..end]
    );
    if end < text.len() {
        out.push_str(&format!(
            "\n\n[… {} more characters. Continue with offset={end}]",
            text.len() - end
        ));
    }
    let mut names: Vec<&str> = Vec::new();
    for name in crate::engine::note_asset_refs(&text[start..end]) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut pictures = Vec::new();
    if !names.is_empty() && ctx.host.sees_images(&ctx.session_id) {
        let mut bytes = 0;
        for name in names.iter().take(READ_PICTURES) {
            if let Some((mime, data)) = ctx.host.note_picture(&ctx.session_id, &n.id, name)
                && bytes + data.len() <= READ_PICTURE_BYTES
            {
                bytes += data.len();
                pictures.push(ContentPart::text(format!("assets/{name}:")));
                pictures.push(ContentPart::Image {
                    mime: mime.into(),
                    data: base64::engine::general_purpose::STANDARD.encode(&data),
                });
            }
        }
    }
    if !names.is_empty() {
        out.push_str(&match pictures.len() / 2 {
            0 => format!(
                "\n\n[{} picture(s) in this part; this model doesn't see pictures.]",
                names.len()
            ),
            k if k < names.len() => {
                format!("\n\n[The first {k} of {} pictures follow.]", names.len())
            }
            _ => "\n\n[Its pictures follow.]".into(),
        });
    }
    let mut out = ToolOutput::text(out);
    out.content.extend(pictures);
    if scan.is_clean() {
        out
    } else {
        out.with_details(json!({"injection": format!("note \"{}\": {}", n.title, scan.summary())}))
    }
}

/// A picture file kept for notes: added to the end of note `id`, or its
/// markdown handed back to be placed.
async fn image(args: &Value, id: Option<&str>, ctx: &ToolCtx) -> Result<(String, String), String> {
    let path = ctx.resolve(opt_str(args, "path").ok_or("`path` (a picture file) is required.")?);
    let bytes =
        super::files::authorized_bytes(&path, ctx, crate::engine::NOTE_PICTURE_MAX as u64).await?;
    let name = ctx.host.note_picture_put(&ctx.session_id, &bytes)?;
    let alt: String = opt_str(args, "alt")
        .unwrap_or("")
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '\n' | '\r'))
        .collect();
    let md = format!("![{}](assets/{name})", alt.trim());
    let Some(id) = id else {
        return Ok((
            format!("Kept the picture. Put {md} where it belongs in a note."),
            String::new(),
        ));
    };
    for _ in 0..2 {
        let n = ctx
            .host
            .note(&ctx.session_id, id)
            .ok_or("No such note; `list` shows them.")?;
        let sep = if n.content.is_empty() || n.content.ends_with("\n\n") {
            ""
        } else if n.content.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let content = format!("{}{sep}{md}\n", n.content);
        match ctx.host.note_save(
            &ctx.session_id,
            Some(id),
            &n.title,
            content,
            Some(n.version),
        ) {
            Ok(n) => {
                let text = format!(
                    "Added the picture to note {} \"{}\" (version {}) as {md}.",
                    n.id, n.title, n.version
                );
                return Ok((text, n.id));
            }
            Err(e) if e.contains("changed") => {}
            Err(e) => return Err(e),
        }
    }
    Err("the note keeps changing; try again".into())
}

/// Replace one exact passage (it must appear once) with another; the title
/// stays unless a new one is given. Saved over whatever is current, so the
/// user's edits elsewhere in the note stay.
fn edit(args: &Value, id: Option<&str>, ctx: &ToolCtx) -> Result<(String, String), String> {
    let id = id.ok_or("`id` is required.")?;
    let find = opt_str(args, "find").filter(|f| !f.is_empty());
    let (Some(find), Some(replace)) = (find, opt_str(args, "replace")) else {
        return Err("`find` (exact text from the note) and `replace` are required.".into());
    };
    for _ in 0..2 {
        let n = ctx
            .host
            .note(&ctx.session_id, id)
            .ok_or("No such note; `list` shows them.")?;
        let content = match n.content.matches(find).count() {
            0 => {
                return Err(
                    "`find` isn't in the note (it must match exactly); `read` it again.".into(),
                );
            }
            1 => n.content.replacen(find, replace, 1),
            k => {
                return Err(format!(
                    "`find` appears {k} times; include more of the text around it so it's unique."
                ));
            }
        };
        let title = opt_str(args, "title").unwrap_or(&n.title);
        match ctx
            .host
            .note_save(&ctx.session_id, Some(id), title, content, Some(n.version))
        {
            Ok(n) => {
                let text = format!(
                    "Edited note {} \"{}\" (version {}).",
                    n.id, n.title, n.version
                );
                return Ok((text, n.id));
            }
            // Someone saved in between: look again.
            Err(e) if e.contains("changed") => {}
            Err(e) => return Err(e),
        }
    }
    Err("the note keeps changing; try again".into())
}

/// Replace the content (checked against `version` when given) or append to
/// it; the title stays unless a new one is given.
fn update(args: &Value, id: Option<&str>, ctx: &ToolCtx) -> Result<(String, String), String> {
    let id = id.ok_or("`id` is required.")?;
    for attempt in 0..2 {
        let n = ctx
            .host
            .note(&ctx.session_id, id)
            .ok_or("No such note; `list` shows them.")?;
        let title = opt_str(args, "title").unwrap_or(&n.title);
        let (content, base) = match (opt_str(args, "content"), opt_str(args, "append")) {
            (Some(c), None) => (c.to_string(), opt_u64(args, "version").or(Some(n.version))),
            (None, Some(a)) => {
                let sep = if n.content.is_empty() || n.content.ends_with("\n\n") {
                    ""
                } else if n.content.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                (
                    format!("{}{sep}{}", n.content, a.trim_end()),
                    Some(n.version),
                )
            }
            _ => return Err("Give either `content` (replaces) or `append`.".into()),
        };
        match ctx
            .host
            .note_save(&ctx.session_id, Some(id), title, content, base)
        {
            Ok(n) => {
                let text = format!(
                    "Updated note {} \"{}\" (version {}).",
                    n.id, n.title, n.version
                );
                return Ok((text, n.id));
            }
            // Someone saved in between: an append simply tries again.
            Err(e)
                if e.contains("changed") && opt_str(args, "append").is_some() && attempt == 0 => {}
            Err(e) if e.contains("changed") => {
                return Err(format!(
                    "{e}: `read` it again (the user may have edited it), then update."
                ));
            }
            Err(e) => return Err(e),
        }
    }
    Err("the note keeps changing; try again".into())
}
