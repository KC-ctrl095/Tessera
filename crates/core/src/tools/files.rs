//! read / write / edit.
//!
//! Reads show a file tag (`[path#3F9A]`, a 16-bit hash of the content) and
//! numbered lines. Edits can then target line ranges without retyping the old
//! text; the tag proves the model is editing the version it saw. Classic
//! old/new string replacement is also supported for models that prefer it.
//!
//! Edit engine v2:
//! - **Stale-tag rebase.** An edit against an older tag is applied when every
//!   line it targets is unchanged in the current file (followed through a
//!   line diff, with a small window for blocks the diff couldn't place);
//!   otherwise it is refused with the current text of the region.
//! - **Seen lines.** Line-anchored edits may only target lines some read,
//!   grep, edit or `lsp` result showed this session (see `filecache.rs`); a
//!   refusal shows the lines so the retry is informed.
//! - **Replace chain.** exact → ignoring indentation (`new` re-indented to
//!   fit) → lines trimmed with spaces collapsed; a loose match must be unique
//!   (or `all`) and not much longer than `old`.
//! - **After writing**: JSON/TOML/YAML files are parse-checked and language
//!   servers report the errors the change introduced (`lsp/`).

use std::path::Path;

use base64::Engine;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{ContentPart, plain_text};

use super::filecache::{FileCache, RangeSet};
use super::images;
use super::linemap::{LineMap, Mapped, split_lines};
use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str, opt_u64};

const DEFAULT_LIMIT: usize = 1000;
const MAX_LINE: usize = 2000;
const MAX_READ_BYTES: usize = 96 * 1024;
/// Existing content checked before a whole-file write, including after approval.
const MAX_WRITE_BYTES: u64 = 16 << 20;
/// Lines of current text shown when an edit is refused.
const EXCERPT_LINES: usize = 40;
/// How far (in lines) a rebase looks for a block the diff couldn't place.
const REBASE_WINDOW: usize = 10;

/// 4-hex content tag. Trailing whitespace is ignored so editor noise does
/// not invalidate a read.
pub fn file_tag(content: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for line in content.split_inclusive('\n') {
        let body = line
            .trim_end_matches(['\n'])
            .trim_end_matches([' ', '\t', '\r']);
        for b in body.bytes().chain(std::iter::once(b'\n')) {
            h ^= b as u32;
            h = h.wrapping_mul(0x01000193);
        }
    }
    format!("{:04X}", (h ^ (h >> 16)) & 0xffff)
}

fn is_image(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Check the object opened for this operation, not an earlier resolution.
/// A different approval target requires a new call; never silently widen it.
pub(crate) fn startup_path(path: &Path) -> bool {
    let canon = super::canonical(path);
    crate::node::server_startup_paths(dirs::home_dir().as_deref())
        .iter()
        .any(|p| canon.starts_with(super::canonical(p)))
}

pub(crate) fn startup_write(tool: &str, args: &Value, ctx: &ToolCtx) -> bool {
    matches!(tool, "write" | "edit")
        && ctx.host.sandbox().is_some()
        && opt_str(args, "path").is_some_and(|p| startup_path(&ctx.resolve(p)))
}

pub(crate) fn authorize_file(path: &Path, ctx: &ToolCtx, write: bool) -> Result<(), String> {
    let mode = ctx.host.permission(&ctx.session_id);
    if write && mode == tessera_proto::PermissionMode::ReadOnly {
        return Err("This chat is read-only; the file was not changed.".into());
    }
    let risk = if write {
        opened_write_risk(path, path, ctx)
    } else {
        super::opened_path_risk(path, ctx)
    };
    match risk {
        Risk::Block { reason } => Err(reason),
        Risk::Ask { key, .. }
            if (mode != tessera_proto::PermissionMode::Auto
                || (write && ctx.host.sandbox().is_some() && startup_path(path)))
                && ctx.cleared.as_deref() != Some(&key) =>
        {
            Err(
                "The file target needs a different approval; call the tool again for this path."
                    .into(),
            )
        }
        _ => Ok(()),
    }
}

async fn opened_bytes(
    path: &Path,
    ctx: &ToolCtx,
    write: bool,
) -> Result<(super::file_object::Opened, Vec<u8>), String> {
    let file = super::file_object::Opened::open(path)
        .map_err(|e| format!("Cannot open {}: {e}", ctx.display(path)))?;
    authorize_file(&file.path, ctx, write)?;
    let bytes = file.bytes().await.map_err(|e| e.to_string())?;
    Ok((file, bytes))
}
/// Raw host-side reads for checks/imports must not reopen an assessed name.
/// Unlike `read`, these consumers cannot redact a credential file, so refuse it.
pub(crate) async fn authorized_bytes(
    path: &Path,
    ctx: &ToolCtx,
    max: u64,
) -> Result<Vec<u8>, String> {
    let path = path.to_path_buf();
    let ctx = ctx.clone();
    tokio::task::spawn_blocking(move || authorized_bytes_sync(&path, &ctx, max))
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) fn authorized_bytes_sync(
    path: &Path,
    ctx: &ToolCtx,
    max: u64,
) -> Result<Vec<u8>, String> {
    let opened = super::file_object::Opened::open(path).map_err(|e| e.to_string())?;
    authorized_opened_bytes(&opened, ctx, max)
}

pub(crate) fn authorized_opened_bytes(
    opened: &super::file_object::Opened,
    ctx: &ToolCtx,
    max: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    authorize_file(&opened.path, ctx, false)?;
    if super::credential_file(&opened.path) {
        return Err("Credential files cannot be copied by this operation.".into());
    }
    opened.check_path().map_err(|e| e.to_string())?;
    let meta = opened.file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max {
        return Err("Not a regular file within the operation's size limit.".into());
    }
    if ctx.cancel.is_cancelled() {
        return Err("The read was cancelled.".into());
    }
    let mut bytes = Vec::new();
    (&opened.file)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    opened.check_path().map_err(|e| e.to_string())?;
    if ctx.cancel.is_cancelled() || bytes.len() as u64 > max {
        return Err("The file changed or the read was cancelled.".into());
    }
    Ok(bytes)
}

pub(crate) async fn authorized_text(path: &Path, ctx: &ToolCtx) -> Result<String, String> {
    String::from_utf8(authorized_bytes(path, ctx, 16 * 1024 * 1024).await?)
        .map_err(|e| e.to_string())
}

pub struct Read;

impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> String {
        "Read text files as `[path#TAG]` plus numbered `N:content` lines (the TAG and numbers are what `edit` \
         takes), view an image, or list a directory. `offset` (1-based) and `limit` (default 1000) page \
         through large files."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "paths": {"type": "array", "items": {"type": "string"}},
                "offset": {"type": "integer"},
                "limit": {"type": "integer"}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, args: &Value) -> String {
        read_paths(args).join(", ")
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let paths = read_paths(args);
        let mut ask = None;
        for p in &paths {
            match super::path_risk(&ctx.resolve(p), ctx) {
                Risk::Block { reason } if paths.len() > 1 => {
                    return Risk::Block {
                        reason: format!("{p}: {reason}"),
                    };
                }
                block @ Risk::Block { .. } => return block,
                // One approval names one file: several that each need
                // one are read one at a time.
                Risk::Ask { .. } if ask.is_some() => {
                    return Risk::Block {
                        reason: "Several of these files need the user's approval; read each of them on its own.".into(),
                    };
                }
                a @ Risk::Ask { .. } => ask = Some(a),
                Risk::None => {}
            }
        }
        ask.unwrap_or(Risk::None)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let paths = read_paths(&args);
            // Stop where the engine would cut the result (losing its middle):
            // the rest is a `Continue with offset` away.
            let max_bytes = (2 * ctx.host.tool_output_limit(&ctx.session_id))
                .saturating_sub(1024)
                .clamp(2048, MAX_READ_BYTES);
            let mut out = match paths.as_slice() {
                [] => return ToolOutput::error("Give `path` (or `paths` for several files)."),
                [one] => read_path(one, &args, ctx, max_bytes).await,
                many if many.len() > MAX_PATHS => {
                    return ToolOutput::error(format!(
                        "At most {MAX_PATHS} files per call; read the others in another call."
                    ));
                }
                many => read_many(many, &args, ctx, max_bytes).await,
            };
            super::attach_late_diagnostics(ctx, &mut out);
            out
        })
    }
}

/// Most files one `read` takes.
const MAX_PATHS: usize = 20;
/// Most pictures one `read` of several files shows.
const MAX_PICTURES: usize = 3;

/// The files a `read` call names: `paths`, else `path` (a list under
/// `path`, or one name under `paths`, is taken too), without repeats.
fn read_paths(args: &Value) -> Vec<String> {
    let list = |v: Option<&Value>| -> Vec<String> {
        match v {
            Some(Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut out = list(args.get("path"));
    for p in list(args.get("paths")) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// How `budget` bytes of lines are shared by files of about `sizes` bytes
/// (`None`: nothing to share, e.g. a file that couldn't be opened):
/// smallest first, each gets what it needs up to an even share of what is
/// left, so small files come whole and one big file can't crowd out the
/// rest. The shares never add up to more than `budget`.
fn shares(sizes: &[Option<u64>], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&i| sizes[i].is_some()).collect();
    order.sort_by_key(|&i| sizes[i]);
    let mut out = vec![0; sizes.len()];
    let mut left = budget;
    for (k, &i) in order.iter().enumerate() {
        // Line numbers add to the file (much for short lines).
        let need = (sizes[i].unwrap_or(0) as usize).saturating_mul(3) / 2 + 256;
        let share = need.min(left / (order.len() - k));
        out[i] = share;
        left -= share;
    }
    out
}

/// What one file of a read of several takes besides its lines: its header
/// (`[path#TAG] lines …`), the "Continue with offset" line and the blank
/// line between files, or a short message instead (an error, a picture, a
/// binary file); on top of the file's name shown.
const PART_BYTES: usize = 200;
/// The line for a file left out of a read of several, on top of its name.
const LEFT_OUT_BYTES: usize = 100;
/// Lines a file gets at least when it is read with others: more than one
/// full line (lines are cut at 2,000 bytes), so the first line always fits.
const MIN_LINES_BYTES: usize = 2_100;
/// What a directory's listing is planned at in a read of several (it is
/// cut to its share).
const LISTING_BYTES: u64 = 8 * 1024;

/// How many of the files of a read of several (in the order given) fit in
/// `max_bytes`, and each one's share for its lines: every included file's
/// `fixed` part and every left-out file's line are paid first, then each
/// file with lines (`sizes[i]` is `Some`) gets at least
/// [`MIN_LINES_BYTES`]. `None` when not even the first one fits.
fn plan_reads(
    fixed: &[usize],
    left_out: &[usize],
    sizes: &[Option<u64>],
    max_bytes: usize,
) -> Option<(usize, Vec<usize>)> {
    (1..=fixed.len()).rev().find_map(|k| {
        let cost = fixed[..k].iter().sum::<usize>() + left_out[k..].iter().sum::<usize>();
        let with_lines = sizes[..k].iter().filter(|s| s.is_some()).count();
        (cost + with_lines * MIN_LINES_BYTES <= max_bytes)
            .then(|| (k, shares(&sizes[..k], max_bytes - cost)))
    })
}

/// Several files in one result: each as its own read would show it (tag,
/// numbered lines), one after the other, within the size one read may take
/// (headers included, see [`plan_reads`]), so the engine never cuts the
/// result. Files that don't fit are left out with a line saying so, and
/// only what the result really shows counts as seen. Each file is opened
/// and authorized once, and the object opened is the one sized and read. A
/// file that can't be read says so and the others still come.
async fn read_many(paths: &[String], args: &Value, ctx: &ToolCtx, max_bytes: usize) -> ToolOutput {
    let opened: Vec<Result<OpenedRead, ToolOutput>> =
        paths.iter().map(|p| open_for_read(p, ctx)).collect();
    let names: Vec<String> = paths.iter().map(|p| ctx.display(&ctx.resolve(p))).collect();
    let fixed: Vec<usize> = opened
        .iter()
        .zip(&names)
        .map(|(o, name)| match o {
            Ok(_) => name.len() + PART_BYTES,
            Err(e) => plain_text(&e.content).len() + 2,
        })
        .collect();
    let left_out: Vec<usize> = names.iter().map(|n| n.len() + LEFT_OUT_BYTES).collect();
    let sizes: Vec<Option<u64>> = opened
        .iter()
        .map(|o| match o {
            Ok(r) if r.meta.is_dir() => Some(LISTING_BYTES),
            Ok(r) if is_image(&r.path).is_some() => None,
            Ok(r) => Some(r.meta.len()),
            Err(_) => None,
        })
        .collect();
    let Some((count, shares)) = plan_reads(&fixed, &left_out, &sizes, max_bytes) else {
        return ToolOutput::error(format!(
            "These {} files are too much for one read here; read fewer at once.",
            paths.len()
        ));
    };
    let mut parts = Vec::with_capacity(count);
    for (one, share) in opened.into_iter().take(count).zip(shares) {
        parts.push(match one {
            Ok(r) => read_opened(r, args, ctx, share).await,
            Err(e) => (e, None),
        });
    }
    // Count the text for pictures left out too, before the size check.
    let mut pictures = 0;
    for (one, _) in &mut parts {
        for part in &mut one.content {
            if matches!(part, ContentPart::Image { .. }) {
                if pictures >= MAX_PICTURES {
                    *part = ContentPart::text(" (not shown: read it on its own to see it)");
                }
                pictures += 1;
            }
        }
    }
    // Whatever still doesn't fit is left out before anything counts as seen.
    let left_line = |name: &str| {
        format!("{name}: not read here (the result would be too long); read it in another call.")
    };
    let size_of = |parts: &[(ToolOutput, Option<Shown>)], from: usize| {
        parts
            .iter()
            .map(|(o, _)| plain_text(&o.content).len() + 2)
            .sum::<usize>()
            + names[from..]
                .iter()
                .map(|n| left_line(n).len() + 2)
                .sum::<usize>()
    };
    while !parts.is_empty() && size_of(&parts, parts.len()) > max_bytes {
        parts.pop();
    }
    if parts.is_empty() {
        return ToolOutput::error(format!(
            "These {} files are too much for one read here; read fewer at once.",
            paths.len()
        ));
    }
    let shown_count = parts.len();
    let mut text = String::new();
    let mut images = Vec::new();
    let mut failed = 0;
    for (one, shown) in parts {
        if let Some(s) = shown {
            s.record(ctx);
        }
        failed += usize::from(one.is_error);
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        for part in one.content {
            match part {
                ContentPart::Text { text: t } => text.push_str(&t),
                other => images.push(other),
            }
        }
    }
    for name in &names[shown_count..] {
        text.push_str("\n\n");
        text.push_str(&left_line(name));
    }
    let mut content = vec![ContentPart::text(text)];
    content.extend(images);
    ToolOutput {
        content,
        is_error: failed == shown_count,
        details: None,
    }
}

/// A file (or directory) of a `read`, opened and authorized.
struct OpenedRead {
    path: std::path::PathBuf,
    opened: super::file_object::Opened,
    meta: std::fs::Metadata,
}

/// What a read showed of a text file, recorded as seen (and the language
/// server warmed) only once it is certain to reach the model.
struct Shown {
    path: std::path::PathBuf,
    tag: String,
    text: String,
    lines: Option<(usize, usize)>,
}

impl Shown {
    fn record(self, ctx: &ToolCtx) {
        if let (Some(cache), Some(lines)) = (ctx.host.file_cache(), self.lines) {
            cache.saw(
                &ctx.session_id,
                &self.path,
                &self.tag,
                Some(&self.text),
                [lines],
            );
        }
        // Warm the language server so an edit has a baseline to compare with.
        if let Some((lsp, env)) = super::lsp_env(ctx) {
            lsp.touch(&env, &self.path, &self.text);
        }
    }
}

fn open_for_read(raw: &str, ctx: &ToolCtx) -> Result<OpenedRead, ToolOutput> {
    let path = ctx.resolve(raw);
    let opened = super::file_object::Opened::open(&path)
        .map_err(|e| ToolOutput::error(format!("Cannot read {}: {e}", ctx.display(&path))))?;
    authorize_file(&opened.path, ctx, false).map_err(ToolOutput::error)?;
    let meta = opened
        .file
        .metadata()
        .map_err(|e| ToolOutput::error(e.to_string()))?;
    Ok(OpenedRead { path, opened, meta })
}

/// One file (or directory, or picture) of a `read`, shown within about
/// `max_bytes`.
async fn read_path(raw: &str, args: &Value, ctx: &ToolCtx, max_bytes: usize) -> ToolOutput {
    match open_for_read(raw, ctx) {
        Ok(r) => {
            let (out, shown) = read_opened(r, args, ctx, max_bytes).await;
            if let Some(s) = shown {
                s.record(ctx);
            }
            out
        }
        Err(e) => e,
    }
}

/// One opened file of a `read` within about `max_bytes`, and for a text
/// file what it showed (for the caller to record).
async fn read_opened(
    r: OpenedRead,
    args: &Value,
    ctx: &ToolCtx,
    max_bytes: usize,
) -> (ToolOutput, Option<Shown>) {
    let OpenedRead { path, opened, meta } = r;
    if meta.is_dir() {
        let result = list_dir(&opened.listing_path(), &path, ctx, max_bytes).await;
        if let Err(e) = opened.check_path() {
            return (ToolOutput::error(e.to_string()), None);
        }
        return (result, None);
    }
    if let Some(mime) = is_image(&path) {
        let unfit = |why: String| {
            ToolOutput::error(format!(
                "{} can't be shown to the model: {why}. {}",
                ctx.display(&path),
                images::SHRINK_HINT
            ))
        };
        if meta.len() > images::MAX_BYTES as u64 {
            return (
                unfit(format!(
                    "it is {} and models take at most {} per picture",
                    crate::util::human_bytes(meta.len()),
                    crate::util::human_bytes(images::MAX_BYTES as u64)
                )),
                None,
            );
        }
        let bytes = match opened.bytes().await {
            Ok(b) => b,
            Err(e) => return (ToolOutput::error(format!("Cannot read image: {e}")), None),
        };
        if let Some(why) = images::unfit(&bytes) {
            return (unfit(why), None);
        }
        let out = ToolOutput {
            content: vec![
                ContentPart::text(format!(
                    "Image {} ({} bytes):",
                    ctx.display(&path),
                    bytes.len()
                )),
                ContentPart::Image {
                    mime: mime.into(),
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                },
            ],
            is_error: false,
            details: None,
        };
        return (out, None);
    }
    let bytes = match opened.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let out = ToolOutput::error(format!("Cannot read {}: {e}", ctx.display(&path)));
            return (out, None);
        }
    };
    if bytes.iter().take(8192).any(|b| *b == 0) {
        let out = ToolOutput::text(format!(
            "{} is a binary file ({} bytes). Use bash with an appropriate tool (e.g. `file`, `xxd | head`, `pdftotext`) to inspect it.",
            ctx.display(&path),
            bytes.len()
        ));
        return (out, None);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let offset = opt_u64(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = opt_u64(args, "limit")
        .map(|l| l as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, 20_000);
    let view = render_view_within(&ctx.display(&path), &text, offset, limit, max_bytes);
    let shown = Shown {
        path,
        tag: view.tag,
        text,
        lines: view.shown,
    };
    (ToolOutput::text(view.text), Some(shown))
}

struct View {
    text: String,
    tag: String,
    /// 1-based line range that was shown.
    shown: Option<(usize, usize)>,
}

/// `[path#TAG] …` header plus numbered lines.
pub fn render_lines(display: &str, text: &str, offset: usize, limit: usize) -> String {
    render_view(display, text, offset, limit).text
}

fn render_view(display: &str, text: &str, offset: usize, limit: usize) -> View {
    render_view_within(display, text, offset, limit, MAX_READ_BYTES)
}

/// [`render_view`] showing at most about `max_bytes` of lines.
fn render_view_within(
    display: &str,
    text: &str,
    offset: usize,
    limit: usize,
    max_bytes: usize,
) -> View {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let tag = file_tag(text);
    if total == 0 {
        return View {
            text: format!("[{display}#{tag}] (empty file)"),
            tag,
            shown: None,
        };
    }
    if offset > total {
        return View {
            text: format!(
                "[{display}#{tag}] has only {total} lines; offset {offset} is past the end."
            ),
            tag,
            shown: None,
        };
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let numbered = format!("{}:{}\n", i + 1, clip_line(line));
        if !out.is_empty() && out.len() + numbered.len() > max_bytes {
            break;
        }
        out.push_str(&numbered);
        last = i + 1;
    }
    let range = if offset == 1 && last == total {
        format!("{total} lines")
    } else {
        format!("lines {offset}-{last} of {total}")
    };
    let mut result = format!("[{display}#{tag}] {range}\n{out}");
    if last < total {
        result.push_str(&format!(
            "[… {} more lines. Continue with offset={}]",
            total - last,
            last + 1
        ));
    }
    View {
        text: result,
        tag,
        shown: (last >= offset).then_some((offset, last)),
    }
}

fn clip_line(line: &str) -> String {
    if line.len() > MAX_LINE {
        format!(
            "{}… [+{} chars]",
            &line[..crate::util::floor_char(line, MAX_LINE)],
            line.len() - MAX_LINE
        )
    } else {
        line.to_string()
    }
}

/// A directory's entries, folders first, within about `max_bytes`.
async fn list_dir(path: &Path, display_path: &Path, ctx: &ToolCtx, max_bytes: usize) -> ToolOutput {
    let mut rd = match tokio::fs::read_dir(path).await {
        Ok(r) => r,
        Err(e) => {
            return ToolOutput::error(format!("Cannot list {}: {e}", ctx.display(display_path)));
        }
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type().await {
            Ok(t) if t.is_dir() => dirs.push(format!("{name}/")),
            _ => {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                files.push(format!("{name}  ({})", human_size(size)));
            }
        }
    }
    dirs.sort();
    files.sort();
    let total = dirs.len() + files.len();
    let mut out = format!("{} ({} entries)", ctx.display(display_path), total);
    // Room for the closing line.
    let room = max_bytes.saturating_sub(64);
    let mut shown = 0;
    for entry in dirs.into_iter().chain(files).take(500) {
        if out.len() + entry.len() + 1 > room {
            break;
        }
        out.push('\n');
        out.push_str(&entry);
        shown += 1;
    }
    if shown < total {
        out.push_str(&format!(
            "\n[… {} more entries; use glob to filter]",
            total - shown
        ));
    }
    ToolOutput::text(out)
}

pub fn human_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / 1048576.0),
    }
}

// ---- write -------------------------------------------------------------------

pub struct Write;

/// The approval-time assessment is only advisory: reopen, authorize and
/// bound the actual old-content read again before snapshots or replacement.
fn write_target_bytes(
    path: &Path,
    ctx: &ToolCtx,
) -> Result<(super::file_object::Opened, Vec<u8>), String> {
    let opened = super::file_object::Opened::open(path).map_err(|e| e.to_string())?;
    authorize_file(&opened.path, ctx, true)?;
    let bytes = bounded_write_bytes(&opened)?;
    Ok((opened, bytes))
}

fn bounded_write_bytes(opened: &super::file_object::Opened) -> Result<Vec<u8>, String> {
    opened
        .read_bounded(MAX_WRITE_BYTES)
        .map_err(|e| e.to_string())
}

impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> String {
        "Create a file, or overwrite one you have read, with `content` (parent directories are created). Use \
         `edit` to change existing files."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, args: &Value) -> String {
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let risk = write_risk(args, ctx);
        if matches!(risk, Risk::Block { .. }) {
            return risk;
        }
        // Refused before anyone is asked to approve it; the sizes and tag
        // only where no approval is needed to write there.
        match unseen_before_approval(args, ctx, matches!(risk, Risk::None)) {
            Some(reason) => Risk::Block {
                reason: reason.trim_start_matches("Refused: ").to_string(),
            },
            None => risk,
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let path = match arg_str(&args, "path") {
                Ok(p) => ctx.resolve(p),
                Err(e) => return e,
            };
            let content = match arg_str(&args, "content") {
                Ok(c) => c.to_string(),
                Err(e) => return e,
            };
            let display = ctx.display(&path);
            let old = match write_target_bytes(&path, ctx) {
                Ok((_, bytes)) => {
                    if let Some(u) = unseen_write(ctx, &path, &bytes, &content) {
                        return ToolOutput::error(unseen_message(&display, &u, true));
                    }
                    String::from_utf8(bytes).ok()
                }
                Err(_) if !path.exists() => None,
                Err(e) => return ToolOutput::error(e),
            };
            let c = match commit(ctx, &path, &display, old.as_deref(), &content, true).await {
                Ok(c) => c,
                Err(e) => return ToolOutput::error(e),
            };
            let verb = if old.is_some() {
                "Overwrote"
            } else {
                "Created"
            };
            let mut text = format!(
                "[{display}#{}] {verb} ({} lines, +{} −{})",
                c.tag,
                content.lines().count(),
                c.added,
                c.removed
            );
            let after = post_write(ctx, &path, &display, old.as_deref(), &content).await;
            for block in &after.blocks {
                text.push('\n');
                text.push_str(block);
            }
            let mut details = json!({ "path": display, "diff": c.diff });
            after.mark(&mut details);
            let mut out = ToolOutput::text(text).with_details(details);
            super::attach_late_diagnostics(ctx, &mut out);
            out
        })
    }
}

/// Why `write` may not replace `bytes`, what `path` holds now: a non-empty
/// file this chat never read nor wrote, or one that isn't the version it
/// last read or wrote (changed since: the user, a formatter, another
/// helper; or Tessera forgot the read: a restart, an evicted chat, a full
/// cache), so the write could throw away content the agent hasn't seen.
/// Any read of the current version will do, even of part of it or a grep
/// hit in it (Claude Code's rule). `None` without a file cache, for an
/// empty file, or when `content` is what is there already.
enum Unseen {
    Binary,
    Changed {
        before: String,
        after: String,
        /// Lines added and removed since `before`, when its text was kept.
        since: Option<(usize, usize)>,
    },
    Unread {
        lines: usize,
        tag: String,
    },
}

fn unseen_write(ctx: &ToolCtx, path: &Path, bytes: &[u8], content: &str) -> Option<Unseen> {
    let cache = ctx.host.file_cache()?;
    if bytes.iter().all(u8::is_ascii_whitespace) || bytes == content.as_bytes() {
        return None;
    }
    // The tag a read shows (lossy for text that isn't UTF-8).
    let text = String::from_utf8_lossy(bytes);
    let now = file_tag(&text);
    if cache.version(&ctx.session_id, path, &now).is_some() {
        return None;
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Some(Unseen::Binary);
    }
    Some(match cache.latest(&ctx.session_id, path) {
        Some(seen) => Unseen::Changed {
            since: seen
                .content
                .as_deref()
                .map(|before| line_changes(before, &text)),
            before: seen.tag,
            after: now,
        },
        None => Unseen::Unread {
            lines: text.lines().count(),
            tag: now,
        },
    })
}

/// The refusal for [`unseen_write`]; `detailed`: with the file's tag and sizes
/// (not before the user approved a write to a sensitive place).
fn unseen_message(display: &str, u: &Unseen, detailed: bool) -> String {
    const READ_FIRST: &str =
        "Read it first (any part), then `write` it again or `edit` part of it.";
    match u {
        Unseen::Binary => format!(
            "Refused: {display} is a binary file this chat hasn't written; `write` would replace it with text. \
             If that is really what is wanted, say so and replace it with a command."
        ),
        _ if !detailed => format!(
            "Refused: {display} already exists and Tessera has no record of a read of its current version in \
             this chat, so writing could replace content you haven't seen. {READ_FIRST}"
        ),
        Unseen::Changed {
            before,
            after,
            since,
        } => {
            let since = since
                .map(|(added, removed)| format!(": +{added} −{removed} lines"))
                .unwrap_or_default();
            format!(
                "Refused: {display} isn't the version you last read or wrote (#{before} then, #{after} now{since}): \
                 it changed since, or Tessera has no record of a read of this version. Writing would replace what \
                 you haven't seen. Read it again, then `write` it, or better `edit` only what you need to change."
            )
        }
        Unseen::Unread { lines, tag } => format!(
            "Refused: {display} already exists ({lines} lines, #{tag}) and Tessera has no record of a read of its \
             current version in this chat, so writing could replace content you haven't seen. {READ_FIRST}"
        ),
    }
}

/// [`unseen_write`] before the call is approved, so the user isn't asked to
/// approve a write that would be refused (and asked again after the read).
/// Check the opened object, not a second resolution of an assessed name.
/// Only the guard's verdict is returned, never unapproved file contents.
fn unseen_before_approval(args: &Value, ctx: &ToolCtx, detailed: bool) -> Option<String> {
    ctx.host.file_cache()?;
    let path = ctx.resolve(args.get("path")?.as_str()?);
    let content = args.get("content")?.as_str()?;
    let opened = super::file_object::Opened::open(&path).ok()?;
    let risk = opened_write_risk(&opened.path, &path, ctx);
    if let Risk::Block { reason } = risk {
        return Some(reason);
    }
    let detailed = detailed && matches!(risk, Risk::None);
    let meta = opened.file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    // A pre-approval check must not allocate an arbitrarily large existing
    // file. Larger targets need a targeted edit, not a whole-file rewrite.
    if meta.len() > MAX_WRITE_BYTES {
        return Some(format!(
            "Refused: {} is too large to check safely for a full-file write. Read the part you need and use edit instead.",
            ctx.display(&path)
        ));
    }
    // Bound by the opened file's size, including for files larger than the
    // cache's text limit; a racing growth cannot make this read unbounded.
    let bytes = match opened.read_bounded(meta.len()) {
        Ok(bytes) => bytes,
        Err(_) => {
            return Some(format!(
                "Refused: {} could not be checked at its current version. Read it again before writing.",
                ctx.display(&path)
            ));
        }
    };
    let u = unseen_write(ctx, &opened.path, &bytes, content)?;
    Some(unseen_message(&ctx.display(&path), &u, detailed))
}

/// Lines added and removed from `old` to `new` (quick; a big diff gives up
/// early with an approximate count).
fn line_changes(old: &str, new: &str) -> (usize, usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let diff = similar::TextDiff::configure()
        .deadline(deadline)
        .diff_lines(old, new);
    let mut out = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => out.0 += 1,
            similar::ChangeTag::Delete => out.1 += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    out
}

pub(crate) fn write_risk(args: &Value, ctx: &ToolCtx) -> Risk {
    let Some(p) = args.get("path").and_then(Value::as_str) else {
        return Risk::None;
    };
    path_write_risk(&ctx.resolve(p), ctx)
}

/// Writing `path`: blocked for Tessera's own store and the user's read-only
/// folders; asked for sensitive locations, Tessera's skills and (unless the
/// user turned it off) anywhere outside the working directory and the
/// folders the user opened to agents.
pub(crate) fn path_write_risk(path: &Path, ctx: &ToolCtx) -> Risk {
    if let r @ Risk::Block { .. } = super::path_risk(path, ctx) {
        return r;
    }
    let canon = super::canonical(path);
    opened_write_risk(&canon, path, ctx)
}

fn opened_write_risk(canon: &Path, path: &Path, ctx: &ToolCtx) -> Risk {
    if let r @ Risk::Block { .. } = super::opened_path_risk(canon, ctx) {
        return r;
    }
    // Tessera's program and the service files that start it: with the
    // command sandbox on, commands can read them but never change them (a
    // change would run outside the sandbox at the next start), and nor can
    // these tools, whatever the permission mode.
    if ctx.host.sandbox().is_some()
        && crate::node::server_program_paths(dirs::home_dir().as_deref())
            .iter()
            .any(|k| canon.starts_with(super::canonical(k)))
    {
        return Risk::Block {
            reason: format!(
                "Refused: {} is Tessera's own program or a service file that starts it; with the command sandbox on (Settings → Privacy & safety), agents can read it but never change it.",
                canon.display()
            ),
        };
    }
    let cwd_scope = match ctx.host.authorized_cwd(&ctx.session_id) {
        Ok(Some(root)) if root == ctx.cwd => Some(root),
        Ok(Some(_)) => {
            return Risk::Block {
                reason: "The helper's authorized working folder changed; start a new helper."
                    .into(),
            };
        }
        Err(reason) => return Risk::Block { reason },
        Ok(None) => None,
    };
    let access = ctx.host.write_access();
    let rule = access.rule(canon);
    if rule == Some(false) {
        return Risk::Block {
            reason: format!(
                "Refused: {} is in a folder the user made read-only for agents (Settings → Privacy & safety).",
                canon.display()
            ),
        };
    }
    let s = canon.to_string_lossy();
    if sensitive(canon) || (ctx.host.sandbox().is_some() && startup_path(canon)) {
        return Risk::Ask {
            reason: format!("writes to a sensitive location: {s}"),
            key: format!("write:{s}"),
        };
    }
    if canon.starts_with(super::canonical(&ctx.host.user_skills_dir())) {
        return Risk::Ask {
            reason: format!("changes a Tessera skill (instructions later chats load): {s}"),
            key: "write:skills".into(),
        };
    }
    let inside = match cwd_scope {
        Some(root) => canon.starts_with(root),
        None => canon.starts_with(super::canonical(&ctx.cwd)) || path.starts_with(&ctx.cwd),
    };
    if !inside && rule != Some(true) && access.ask_outside {
        let dir = canon
            .parent()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default();
        return Risk::Ask {
            reason: format!("writes outside the working directory: {s}"),
            key: format!("write-dir:{dir}"),
        };
    }
    Risk::None
}

/// System directories, and files that make something run later outside
/// any sandbox or hold keys (shell startup files, autostart, git hooks and
/// config, SSH, programs on the PATH), or change what an agent runs without
/// asking (Claude Code's settings, hooks, skills and MCP servers).
pub(crate) fn sensitive(p: &Path) -> bool {
    const SYSTEM: [&str; 6] = ["/etc", "/usr", "/boot", "/bin", "/sbin", "/lib"];
    if SYSTEM.iter().any(|d| p.starts_with(d)) {
        return true;
    }
    let s = p.to_string_lossy();
    const DIRS: [&str; 11] = [
        "/.ssh/",
        "/.gnupg/",
        "/.aws/",
        "/.config/autostart/",
        "/.config/systemd/",
        "/.local/share/systemd/",
        "/.git/hooks/",
        "/.claude/",
        "/.local/bin/",
        "/.config/fish/",
        "/.config/hypr/",
    ];
    const FILES: [&str; 11] = [
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".profile",
        ".zshrc",
        ".zprofile",
        ".zshenv",
        ".zlogin",
        ".gitconfig",
        ".envrc",
        ".mcp.json",
    ];
    const ENDS: [&str; 2] = ["/.git/config", "/.config/git/config"];
    DIRS.iter().any(|d| s.contains(d))
        || ENDS.iter().any(|e| s.ends_with(e))
        || p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| FILES.contains(&n))
}

/// (+lines, −lines, unified diff)
pub fn diff_stats(old: &str, new: &str, name: &str) -> (usize, usize, String) {
    diff_stats_redacted(old, new, name, &[])
}

fn diff_stats_redacted(
    old: &str,
    new: &str,
    name: &str,
    secrets: &[String],
) -> (usize, usize, String) {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string();
    let unified = super::redact_known(&unified, secrets);
    let unified = if unified.len() > 200_000 {
        crate::util::truncate(&unified, 200_000)
    } else {
        unified
    };
    (added, removed, unified)
}

/// Compact post-edit view: changed regions with *new* line numbers, so the
/// model can keep editing without re-reading. Also returns the new line
/// numbers it showed.
fn change_preview(old: &str, new: &str, secrets: &[String]) -> (String, Vec<usize>) {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = String::new();
    let mut lines = Vec::new();
    let mut shown = 0;
    for group in diff.grouped_ops(2) {
        if shown > 60 {
            out.push_str("…\n");
            break;
        }
        for op in group {
            for change in diff.iter_changes(&op) {
                let text =
                    super::redact_known(change.value().trim_end_matches(['\n', '\r']), secrets);
                let text = crate::util::truncate(&text, 300);
                match change.tag() {
                    similar::ChangeTag::Delete => out.push_str(&format!("-:{text}\n")),
                    _ => {
                        let n = change.new_index().map(|i| i + 1).unwrap_or(0);
                        let mark = if change.tag() == similar::ChangeTag::Insert {
                            "+"
                        } else {
                            " "
                        };
                        out.push_str(&format!("{n}{mark}{text}\n"));
                        lines.push(n);
                    }
                }
                shown += 1;
            }
        }
        out.push_str("…\n");
    }
    (out, lines)
}

/// A write that happened.
pub(crate) struct Committed {
    pub tag: String,
    pub added: usize,
    pub removed: usize,
    pub diff: String,
    pub preview: String,
}

/// Write `new` to `path` the way every file-changing tool must: snapshot
/// the old content (for rewind), write, and record the new version as
/// seen (`whole`: the model wrote all of it; otherwise the lines it changed
/// and the preview lines).
pub(crate) fn prepare_destination(
    path: &Path,
    ctx: &ToolCtx,
) -> Result<super::file_object::Destination, String> {
    // Host locks are taken before the admission gate. Inside the gate only
    // the cloned directory identity is checked; no state/map lock is acquired.
    let scope = ctx.host.authorized_cwd_scope(&ctx.session_id)?;
    if scope.as_ref().is_some_and(|s| s.path() != ctx.cwd) {
        return Err("The helper working folder changed; start a new helper.".into());
    }
    super::file_object::Destination::prepare_scoped(
        path,
        |p| authorize_file(p, ctx, true),
        ctx.admission.clone(),
        scope,
    )
}
pub(crate) async fn commit(
    ctx: &ToolCtx,
    path: &Path,
    display: &str,
    old: Option<&str>,
    new: &str,
    whole: bool,
) -> Result<Committed, String> {
    let destination = prepare_destination(path, ctx)?;
    let _ = ctx.host.store().snapshot_file(
        &ctx.session_id,
        ctx.seq,
        &destination.path.to_string_lossy(),
        old.map(str::as_bytes),
    );
    let before = old.map(str::to_owned);
    let after = new.to_owned();
    tokio::task::spawn_blocking(move || destination.write(before.as_deref(), &after))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Cannot write {display}: {e}"))?;
    let base = old.unwrap_or("");
    let secrets = ctx.host.secrets();
    let (added, removed, diff) = diff_stats_redacted(base, new, display, &secrets);
    let (preview, shown) = change_preview(base, new, &secrets);
    if let Some(cache) = ctx.host.file_cache() {
        if whole {
            cache.wrote(&ctx.session_id, path, old, new, 1..=new.lines().count());
        } else {
            let written = LineMap::new(base, new)
                .changed_lines()
                .map(|i| i + 1)
                .collect::<Vec<_>>();
            cache.wrote(
                &ctx.session_id,
                path,
                old,
                new,
                written.into_iter().chain(shown),
            );
        }
    }
    Ok(Committed {
        tag: file_tag(new),
        added,
        removed,
        diff,
        preview,
    })
}

/// What the checks after a write found.
#[derive(Debug, Default)]
pub(crate) struct AfterWrite {
    /// Warnings and diagnostics for the result.
    pub blocks: Vec<String>,
    /// A language server or a syntax check looked at the new text: whether
    /// it was clean (`engine/verify.rs` counts a clean one as checked).
    pub clean: Option<bool>,
}

impl AfterWrite {
    /// Note the verdict in a result's details (`"checked": "clean"`).
    pub fn mark(&self, details: &mut Value) {
        if let (Some(clean), Some(o)) = (self.clean, details.as_object_mut()) {
            o.insert(
                "checked".into(),
                json!(if clean { "clean" } else { "problems" }),
            );
        }
    }
}

/// Checks after a file changed: a syntax check for data formats, what a
/// language server says the change broke, and where no server covers the
/// file, a quick syntax check with what is installed (`tools/quick.rs`).
pub(crate) async fn post_write(
    ctx: &ToolCtx,
    path: &Path,
    display: &str,
    old: Option<&str>,
    new: &str,
) -> AfterWrite {
    let mut out = AfterWrite::default();
    if let Some(w) = parse_check(path, display, old, new) {
        out.blocks.push(w);
    }
    let mut routed = false;
    if let Some((lsp, env)) = super::lsp_env(ctx) {
        let r = lsp.after_write_checked(&env, path, old, new, true).await;
        routed = r.routed;
        if r.answered {
            out.clean = Some(r.clean());
        }
        out.blocks.extend(r.text);
    }
    if !routed {
        match super::quick::check(ctx, path, display, old, new).await {
            Some(super::quick::Found::Clean) => out.clean = Some(true),
            Some(super::quick::Found::Broken(w)) => {
                out.clean = Some(false);
                out.blocks.push(w);
            }
            None => {}
        }
    }
    out
}

// ---- parse check -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum DataFormat {
    Json,
    Toml,
    Yaml,
}

impl DataFormat {
    fn name(self) -> &'static str {
        match self {
            DataFormat::Json => "JSON",
            DataFormat::Toml => "TOML",
            DataFormat::Yaml => "YAML",
        }
    }
}

/// JSON files whose tools accept comments and trailing commas.
fn jsonc_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    path.extension().is_some_and(|e| e == "jsonc")
        || name.starts_with("tsconfig")
        || name.starts_with("jsconfig")
        || matches!(
            name.as_str(),
            ".eslintrc.json"
                | "devcontainer.json"
                | ".devcontainer.json"
                | "deno.json"
                | ".babelrc.json"
        )
        || path.components().any(|c| c.as_os_str() == ".vscode")
}

/// Why `text` isn't valid `fmt`, or `None` if it is.
fn parse_error(fmt: DataFormat, text: &str, jsonc: bool) -> Option<String> {
    match fmt {
        DataFormat::Json => {
            let strict = serde_json::from_str::<serde::de::IgnoredAny>(text).err()?;
            if jsonc && serde_json::from_str::<serde::de::IgnoredAny>(&strip_jsonc(text)).is_ok() {
                return None;
            }
            Some(strict.to_string())
        }
        DataFormat::Toml => {
            let e = toml::from_str::<toml::Table>(text).err()?;
            let at = e
                .span()
                .map(|s| {
                    let before = &text[..crate::util::floor_char(text, s.start)];
                    let line = before.matches('\n').count() + 1;
                    let col = before
                        .rsplit('\n')
                        .next()
                        .map(|l| l.chars().count())
                        .unwrap_or(0)
                        + 1;
                    format!(" at line {line} column {col}")
                })
                .unwrap_or_default();
            Some(format!("{}{at}", e.message().trim()))
        }
        DataFormat::Yaml => {
            for event in saphyr_parser::Parser::new_from_str(text) {
                if let Err(e) = event {
                    let m = e.marker();
                    return Some(format!(
                        "{} at line {} column {}",
                        e.info(),
                        m.line(),
                        m.col() + 1
                    ));
                }
            }
            None
        }
    }
}

/// A warning when an edit leaves a JSON, TOML or YAML file unparseable.
/// The edit itself stands.
fn parse_check(path: &Path, display: &str, old: Option<&str>, new: &str) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let fmt = match ext.as_str() {
        "json" | "jsonc" => DataFormat::Json,
        "toml" => DataFormat::Toml,
        "yaml" | "yml" => DataFormat::Yaml,
        _ => return None,
    };
    let old_err = old.map(|o| parse_error(fmt, o, true));
    // A file that only parsed leniently before is a JSONC file.
    let jsonc = jsonc_name(path)
        || (fmt == DataFormat::Json
            && old.is_some_and(|o| serde_json::from_str::<serde::de::IgnoredAny>(o).is_err())
            && matches!(old_err, Some(None)));
    let err = parse_error(fmt, new, jsonc)?;
    let name = fmt.name();
    Some(match old_err {
        Some(None) => format!("Warning: {display} no longer parses as {name}: {err}"),
        Some(Some(_)) => format!(
            "Warning: {display} still doesn't parse as {name} (it didn't before this edit either): {err}"
        ),
        None => format!("Warning: {display} doesn't parse as {name}: {err}"),
    })
}

/// JSON with `//` and `/* */` comments and trailing commas removed.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            ',' => {
                // Drop the comma if only whitespace separates it from } or ].
                let rest: String = chars.clone().take_while(|x| x.is_whitespace()).collect();
                let next = chars.clone().nth(rest.chars().count());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// ---- edit ------------------------------------------------------------------

pub struct Edit;

impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> String {
        "Edit a file, one mode per call:\n\
         1. `lines`: items {at, text} against the version you read, with its `tag`. `at` is \"12\" (replace line 12), \
         \"12-15\" (replace a range), \"<12\" / \">12\" (insert before/after line 12) or \">$\" (append); `text` may span \
         lines, \"\" deletes. Numbers refer to the file as read, not shifted by earlier items. Only lines a read or grep \
         showed you can be targeted; an older tag still works if those lines didn't change.\n\
         2. `replace`: items {old, new}; `old` must match once (or set `all`); indentation and spacing differences are \
         tolerated.\n\
         Returns the new tag, changed lines/numbers and new language-server errors."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "tag": {"type": "string"},
                "lines": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"at": {"type": "string"}, "text": {"type": "string"}},
                        "required": ["at", "text"]
                    }
                },
                "replace": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old": {"type": "string"},
                            "new": {"type": "string"},
                            "all": {"type": "boolean"}
                        },
                        "required": ["old", "new"]
                    }
                }
            },
            "required": ["path"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, args: &Value) -> String {
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        if args.get("patch").is_some_and(Value::is_string) {
            return super::patch::assess(args, ctx);
        }
        write_risk(args, ctx)
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            // An `apply_patch` patch (GPT/Codex models; `engine/calls.rs`).
            if let Some(patch) = args.get("patch").and_then(Value::as_str) {
                return super::patch::run(patch, ctx).await;
            }
            let path = match arg_str(&args, "path") {
                Ok(p) => ctx.resolve(p),
                Err(e) => return e,
            };
            let display = ctx.display(&path);
            let old = match opened_bytes(&path, ctx, true).await {
                Ok((_, bytes)) => match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => {
                        return ToolOutput::error(format!(
                            "Cannot read {display}: this is not UTF-8 text. Use `write` to replace it."
                        ));
                    }
                },
                Err(e) => {
                    return ToolOutput::error(format!(
                        "Cannot read {display}: {e}. Use `write` to create new files."
                    ));
                }
            };
            let target = Target {
                session: &ctx.session_id,
                cache: ctx.host.file_cache(),
                path: &path,
                display: &display,
            };
            let mut notes = Vec::new();
            let mut already = Vec::new();
            let result = if let Some(items) = args
                .get("lines")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                let given = args
                    .get("tag")
                    .and_then(Value::as_str)
                    .map(|t| t.trim().trim_start_matches('#').to_ascii_uppercase())
                    .filter(|t| !t.is_empty());
                // No tag, but this session read the file as it is now: that
                // read's tag (the seen-lines rule still applies).
                let tag = given.or_else(|| {
                    let now = file_tag(&old);
                    target
                        .cache
                        .and_then(|c| c.version(&ctx.session_id, &path, &now))
                        .map(|_| {
                            notes.push(format!(
                                "(No `tag` given: used #{now}, from your read of the file as it is now.)"
                            ));
                            now
                        })
                });
                match tag {
                    None => Err(ReplaceError::new(
                        "`lines` edits need the `tag` from a read of this file as it is now (it changed since your last read, or you haven't read it): read it, then use that tag.",
                    )),
                    Some(tag) => target
                        .line_edit(&old, &tag, items)
                        .map(|(new, note)| {
                            notes.extend(note);
                            new
                        })
                        .map_err(ReplaceError::from),
                }
            } else if let Some(items) = args
                .get("replace")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                apply_replacements_ext(&old, items).map(|r| {
                    notes.extend(r.notes);
                    already = r.already;
                    r.text
                })
            } else {
                Err(ReplaceError::new(nothing_to_change(&args)))
            };
            let new = match result {
                Ok(n) => n,
                // Several places matched: end with the call that would work.
                Err(ReplaceError {
                    message,
                    all_of: Some((item, count)),
                }) => {
                    let Some(fix) = with_all(&args, item) else {
                        return ToolOutput::error(format!(
                            "{message} To change all {count}, set \"all\": true in replacement {}.",
                            item + 1
                        ));
                    };
                    return ToolOutput::error(format!(
                        "{message} To change all {count}, send: {fix}"
                    ))
                    .with_details(json!({ "fix": fix }));
                }
                Err(e) => return ToolOutput::error(e.message),
            };
            if new == old && !already.is_empty() {
                // Sent again (a lost result, a retry): nothing to do, and
                // no error to recover from.
                return ToolOutput::text(format!(
                    "No change: {display} already has this edit ({}: `old` isn't there, its `new` text is). Re-read the file if that isn't what you expected.",
                    already.join(", ")
                ))
                .with_details(json!({ "path": display, "unchanged": true }));
            }
            if new == old {
                return ToolOutput::error(
                    "The edit produced no change. Check that the new text differs from the old.",
                );
            }
            let c = match commit(ctx, &path, &display, Some(&old), &new, false).await {
                Ok(c) => c,
                Err(e) => return ToolOutput::error(e),
            };
            let mut text = String::new();
            for n in notes {
                text.push_str(&n);
                text.push('\n');
            }
            text.push_str(&format!(
                "[{display}#{}] edited (+{} −{})\n{}",
                c.tag, c.added, c.removed, c.preview
            ));
            let after = post_write(ctx, &path, &display, Some(&old), &new).await;
            for block in &after.blocks {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(block);
            }
            let mut details = json!({ "path": display, "diff": c.diff });
            after.mark(&mut details);
            let mut out = ToolOutput::text(text.trim_end().to_string()).with_details(details);
            super::attach_late_diagnostics(ctx, &mut out);
            out
        })
    }
}

/// Why an `edit` without `lines` or `replace` items can't run, ending with
/// the call that would, in the model's own terms: text without a place
/// (`text`, `content`, `new` alone) is most likely a whole new version.
fn nothing_to_change(args: &Value) -> String {
    let path = Value::from(args.get("path").and_then(Value::as_str).unwrap_or("…"));
    let replace = format!(
        "{{\"path\": {path}, \"replace\": [{{\"old\": \"<exact text now in the file>\", \"new\": \"<what replaces it>\"}}]}}"
    );
    let has_text = ["text", "content", "new"]
        .iter()
        .any(|k| args.get(*k).is_some_and(Value::is_string));
    if has_text {
        format!(
            "This `edit` has text but nothing that says where it goes. For a whole new version of the file, call \
             `write` with {{\"path\": {path}, \"content\": …}}; to change part of it, send {replace}, or `lines` \
             items {{\"at\": \"12-15\", \"text\": …}} with the `tag` from your latest read."
        )
    } else {
        format!(
            "This `edit` has nothing to change. Send {replace}, or `lines` items {{\"at\": \"12\", \"text\": …}} \
             with the `tag` from your latest read."
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Anchor {
    /// 1-based inclusive range.
    Replace(usize, usize),
    Before(usize),
    After(usize),
    Append,
}

fn parse_anchor(s: &str, total: usize) -> Result<Anchor, String> {
    let s = s.trim();
    let num = |t: &str| -> Result<usize, String> {
        let n: usize = t
            .trim()
            .parse()
            .map_err(|_| format!("bad line number `{t}` in `at`=\"{s}\""))?;
        if n == 0 || n > total.max(1) {
            return Err(format!("line {n} is out of range (file has {total} lines)"));
        }
        Ok(n)
    };
    if s == ">$" || s == "$" {
        return Ok(Anchor::Append);
    }
    if let Some(r) = s.strip_prefix('<') {
        return Ok(Anchor::Before(num(r)?));
    }
    if let Some(r) = s.strip_prefix('>') {
        return Ok(Anchor::After(num(r)?));
    }
    // "12-15", or sed's "12,15"; "$" ends at the last line.
    if let Some((a, b)) = s.split_once('-').or_else(|| s.split_once(',')) {
        let b = if b.trim() == "$" {
            total.max(1)
        } else {
            num(b)?
        };
        let (a, b) = (num(a)?, b);
        if b < a {
            return Err(format!("range \"{s}\" is reversed"));
        }
        return Ok(Anchor::Replace(a, b));
    }
    let n = num(s)?;
    Ok(Anchor::Replace(n, n))
}

/// One parsed `lines` item.
#[derive(Debug, Clone)]
struct Op {
    anchor: Anchor,
    lines: Vec<String>,
    /// Position in the request (for messages).
    index: usize,
    at: String,
}

fn parse_ops(items: &[Value], total: usize) -> Result<Vec<Op>, String> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let at = item
                .get("at")
                .and_then(Value::as_str)
                .ok_or(format!("item {} is missing `at`", i + 1))?;
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or(format!("item {} is missing `text`", i + 1))?;
            let lines = if text.is_empty() {
                Vec::new()
            } else {
                text.trim_end_matches('\n')
                    .split('\n')
                    .map(|l| l.trim_end_matches('\r').to_string())
                    .collect()
            };
            Ok(Op {
                anchor: parse_anchor(at, total)?,
                lines,
                index: i,
                at: at.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
fn apply_line_edits(old: &str, items: &[Value]) -> Result<String, String> {
    apply_ops(old, parse_ops(items, old.lines().count())?)
}

fn apply_ops(old: &str, ops: Vec<Op>) -> Result<String, String> {
    let crlf = old.contains("\r\n");
    let trailing_nl = old.ends_with('\n');
    let lines: Vec<&str> = old.lines().collect();
    let total = lines.len();
    // (start index, end index exclusive, new lines, order)
    let mut spans: Vec<(usize, usize, Vec<String>, usize)> = ops
        .into_iter()
        .map(|op| {
            let (s, e) = match op.anchor {
                Anchor::Replace(a, b) => (a - 1, b),
                Anchor::Before(n) => (n - 1, n - 1),
                Anchor::After(n) => (n, n),
                Anchor::Append => (total, total),
            };
            (s.min(total), e.min(total), op.lines, op.index)
        })
        .collect();
    spans.sort_by_key(|(s, e, _, i)| (*s, *e, *i));
    for w in spans.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.0 < a.1 {
            return Err(format!(
                "edits {} and {} overlap; combine them into one item",
                a.3 + 1,
                b.3 + 1
            ));
        }
    }
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    for (s, e, new_lines, _) in spans.into_iter().rev() {
        out.splice(s..e, new_lines);
    }
    let nl = if crlf { "\r\n" } else { "\n" };
    let mut joined = out.join(nl);
    if (trailing_nl || total == 0) && !joined.is_empty() {
        joined.push_str(nl);
    }
    Ok(joined)
}

/// Lines an item needs to have been shown: a replaced range, or the line an
/// insert is anchored to (not for the very start or end of the file).
fn unseen(seen: &RangeSet, ops: &[Op], total: usize) -> Vec<(usize, usize)> {
    let mut missing = RangeSet::default();
    for op in ops {
        let (a, b) = match op.anchor {
            Anchor::Replace(a, b) => (a, b),
            Anchor::Before(n) if n > 1 => (n, n),
            Anchor::After(n) if n < total => (n, n),
            _ => continue,
        };
        for (x, y) in seen.missing(a, b) {
            missing.insert(x, y);
        }
    }
    missing.ranges().to_vec()
}

/// Where a block of old lines is in the current text (1-based inclusive):
/// followed through the diff when every line is unchanged and still
/// contiguous, else the identical block found uniquely near where it should be.
fn place_block(
    map: &LineMap,
    old: &[&str],
    cur: &[&str],
    s: usize,
    e: usize,
) -> Option<(usize, usize)> {
    let moved: Option<Vec<usize>> = (s..=e)
        .map(|n| match map.map(n - 1)? {
            Mapped::Same(m) => Some(m),
            Mapped::Changed(_) => None,
        })
        .collect();
    if let Some(ms) = moved
        && ms.windows(2).all(|w| w[1] == w[0] + 1)
    {
        return Some((ms[0] + 1, ms[ms.len() - 1] + 1));
    }
    let len = e - s + 1;
    if cur.len() < len {
        return None;
    }
    let block = &old[s - 1..e];
    let expected = map.map(s - 1)?.line();
    let from = expected.saturating_sub(REBASE_WINDOW);
    let to = (expected + REBASE_WINDOW).min(cur.len() - len);
    let hits: Vec<usize> = (from..=to).filter(|&i| cur[i..i + len] == *block).collect();
    match hits.as_slice() {
        [i] => Some((i + 1, i + len)),
        _ => None,
    }
}

/// An item that can't be rebased, and the region of the current text to show.
type Conflict = (Op, (usize, usize));

/// Translate items written against `old` to `cur`.
fn rebase(old: &str, cur: &str, ops: &[Op]) -> Result<Vec<Op>, Vec<Conflict>> {
    let ol = split_lines(old);
    let cl = split_lines(cur);
    let map = LineMap::from_lines(&ol, &cl);
    let mut out = Vec::new();
    let mut conflicts = Vec::new();
    for op in ops {
        let (s, e) = match op.anchor {
            Anchor::Append => {
                out.push(op.clone());
                continue;
            }
            Anchor::Replace(a, b) => (a, b),
            Anchor::Before(n) | Anchor::After(n) => (n, n),
        };
        let placed = place_block(&map, &ol, &cl, s, e).map(|(a, b)| match op.anchor {
            Anchor::Replace(..) => Anchor::Replace(a, b),
            Anchor::Before(_) => Anchor::Before(a),
            Anchor::After(_) => Anchor::After(b),
            Anchor::Append => Anchor::Append,
        });
        match placed {
            Some(anchor) => out.push(Op {
                anchor,
                ..op.clone()
            }),
            None => {
                let at = |n: usize| map.map(n - 1).map(|m| m.line() + 1).unwrap_or(1);
                let (a, b) = (at(s), at(e));
                conflicts.push((op.clone(), (a.min(b), a.max(b))));
            }
        }
    }
    if conflicts.is_empty() {
        Ok(out)
    } else {
        Err(conflicts)
    }
}

fn describe_ranges(ranges: &[(usize, usize)]) -> String {
    let parts: Vec<String> = ranges
        .iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect();
    let single = ranges.len() == 1 && ranges[0].0 == ranges[0].1;
    format!(
        "{} {}",
        if single { "line" } else { "lines" },
        parts.join(", ")
    )
}

/// The file an edit targets, and what the session knows about it.
struct Target<'a> {
    session: &'a str,
    cache: Option<&'a FileCache>,
    path: &'a Path,
    display: &'a str,
}

impl Target<'_> {
    /// Apply `lines` items written against version `tag` to `current`.
    /// Returns the new text and an optional note for the result.
    fn line_edit(
        &self,
        current: &str,
        tag: &str,
        items: &[Value],
    ) -> Result<(String, Option<String>), String> {
        let cur_tag = file_tag(current);
        if tag == cur_tag {
            let total = current.lines().count();
            let ops = parse_ops(items, total)?;
            if let Some(cache) = self.cache {
                let seen = cache
                    .version(self.session, self.path, tag)
                    .map(|v| v.seen)
                    .unwrap_or_default();
                let missing = unseen(&seen, &ops, total);
                if !missing.is_empty() {
                    return Err(self.refuse_unseen(current, &cur_tag, &missing));
                }
            }
            return apply_ops(current, ops).map(|n| (n, None));
        }
        let known = self
            .cache
            .and_then(|c| c.version(self.session, self.path, tag))
            .and_then(|v| v.content.clone().map(|c| (v.seen, c)));
        let Some((seen, old)) = known else {
            return Err(self.stale_unknown(current, &cur_tag, tag, items));
        };
        let old_total = old.lines().count();
        let ops = parse_ops(items, old_total)?;
        let missing = unseen(&seen, &ops, old_total);
        if !missing.is_empty() {
            // Never shown even in that version: show where those lines are now.
            let map = LineMap::new(&old, current);
            let now: Vec<(usize, usize)> = missing
                .iter()
                .map(|&(a, b)| {
                    let at = |n: usize| map.map(n - 1).map(|m| m.line() + 1).unwrap_or(1);
                    (at(a), at(b).max(at(a)))
                })
                .collect();
            return Err(self.refuse_unseen(current, &cur_tag, &now));
        }
        match rebase(&old, current, &ops) {
            Ok(moved) => apply_ops(current, moved).map(|n| {
                (
                    n,
                    Some(format!(
                        "(Rebased from #{tag}: the lines you targeted were unchanged.)"
                    )),
                )
            }),
            Err(conflicts) => Err(self.conflict(current, &cur_tag, tag, &conflicts)),
        }
    }

    /// Current text of `ranges` (±1 line), at most [`EXCERPT_LINES`] lines,
    /// recorded as seen. Returns the text and whether it was cut short.
    fn excerpt(
        &self,
        current: &str,
        cur_tag: &str,
        ranges: &[(usize, usize)],
    ) -> (String, Option<usize>) {
        let lines: Vec<&str> = current.lines().collect();
        let total = lines.len();
        let mut want = RangeSet::default();
        for &(a, b) in ranges {
            want.insert(a.saturating_sub(1).max(1), (b + 1).min(total.max(1)));
        }
        let mut out = String::new();
        let mut shown = Vec::new();
        let mut count = 0;
        let mut cut = None;
        'ranges: for &(a, b) in want.ranges() {
            if !out.is_empty() {
                out.push_str("…\n");
            }
            for n in a..=b.min(total) {
                if count == EXCERPT_LINES {
                    cut = Some(n);
                    break 'ranges;
                }
                out.push_str(&format!("{n}:{}\n", clip_line(lines[n - 1])));
                shown.push((n, n));
                count += 1;
            }
        }
        if total == 0 {
            out.push_str("(the file is empty)\n");
        }
        if let Some(cache) = self.cache {
            cache.saw(self.session, self.path, cur_tag, Some(current), shown);
        }
        (out, cut)
    }

    fn refuse_unseen(&self, current: &str, cur_tag: &str, missing: &[(usize, usize)]) -> String {
        let (text, cut) = self.excerpt(current, cur_tag, missing);
        let more = cut
            .map(|n| format!("[… read offset={n} for the rest]\n"))
            .unwrap_or_default();
        format!(
            "Refused: {} of {} weren't shown to you by a read or grep since the file last changed, so the edit \
             could be based on a guess. Current text (tag {cur_tag}):\n{text}{more}Check it, then retry with tag {cur_tag}.",
            describe_ranges(missing),
            self.display
        )
    }

    fn stale_unknown(&self, current: &str, cur_tag: &str, tag: &str, items: &[Value]) -> String {
        let total = current.lines().count();
        let ranges: Vec<(usize, usize)> = items
            .iter()
            .filter_map(|i| parse_anchor(i.get("at")?.as_str()?, usize::MAX).ok())
            .filter_map(|a| match a {
                Anchor::Replace(x, y) => Some((x.min(total), y.min(total))),
                Anchor::Before(n) | Anchor::After(n) => Some((n.min(total), n.min(total))),
                Anchor::Append => (total > 0).then_some((total, total)),
            })
            .filter(|(a, _)| *a > 0)
            .collect();
        let (text, _) = self.excerpt(current, cur_tag, &ranges);
        format!(
            "{} changed since that read (tag {tag}, now {cur_tag}). Current text at the lines you targeted:\n{text}\
             Line numbers may have shifted: check, then retry with tag {cur_tag} (or re-read).",
            self.display
        )
    }

    fn conflict(&self, current: &str, cur_tag: &str, tag: &str, conflicts: &[Conflict]) -> String {
        let items: Vec<String> = conflicts
            .iter()
            .map(|(op, _)| format!("item {} at \"{}\"", op.index + 1, op.at))
            .collect();
        let regions: Vec<(usize, usize)> = conflicts.iter().map(|(_, r)| *r).collect();
        let a = regions
            .iter()
            .map(|r| r.0)
            .min()
            .unwrap_or(1)
            .saturating_sub(2)
            .max(1);
        let b = regions.iter().map(|r| r.1).max().unwrap_or(1) + 2;
        let b = b.min(current.lines().count().max(1));
        let (text, cut) = self.excerpt(current, cur_tag, &[(a, b)]);
        let more = cut
            .map(|n| format!("[… read offset={n} for the rest]\n"))
            .unwrap_or_default();
        format!(
            "{} changed since #{tag} (now #{cur_tag}) and the lines you targeted changed too ({}). \
             Re-read lines {a}-{b}; their current text:\n{text}{more}Retry with tag {cur_tag}.",
            self.display,
            items.join(", ")
        )
    }
}

// ---- replace ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Loose {
    /// Trailing whitespace ignored; indentation may differ by a uniform
    /// amount (relative indentation must match).
    Indent,
    /// Every line trimmed, runs of spaces collapsed.
    Trimmed,
}

impl Loose {
    fn describe(self) -> &'static str {
        match self {
            Loose::Indent => "ignoring indentation and trailing spaces",
            Loose::Trimmed => "with lines trimmed and spaces collapsed; check the result",
        }
    }
}

/// One `replace` item.
struct Repl {
    from: String,
    to: String,
    all: bool,
}

/// A byte span of the text and what replaces it.
pub(crate) type Span = (usize, usize, String);

/// Where an item matched: byte spans with their replacement text, and a
/// note when the match was loose.
type Placed = (Vec<Span>, Option<String>);

/// Why an item didn't match.
enum Miss {
    NotFound,
    /// Matches this many places (the message): `all` would take them.
    Ambiguous(String, usize),
    /// A loose match far larger than `old` (the message).
    Refused(String),
}

/// Find `r` in `text`: exactly, then by the loose strategies.
fn place(text: &str, r: &Repl) -> Result<Placed, Miss> {
    let count = text.matches(&r.from).count();
    if count == 1 || (count > 1 && r.all) {
        let spans = text
            .match_indices(&r.from)
            .map(|(pos, m)| (pos, pos + m.len(), r.to.clone()))
            .collect();
        return Ok((spans, None));
    }
    if count > 1 {
        let lines: Vec<usize> = text
            .match_indices(&r.from)
            .map(|(pos, _)| text[..pos].matches('\n').count() + 1)
            .take(8)
            .collect();
        return Err(Miss::Ambiguous(
            format!(
                "`old` matches {count} places (lines {lines:?}). Add surrounding context to make it unique, set `all`, or use `lines`."
            ),
            count,
        ));
    }
    match loose_spans(text, &r.from, &r.to, r.all) {
        Ok((spans, how)) => {
            let places = if spans.len() > 1 {
                format!(" ({} places)", spans.len())
            } else {
                String::new()
            };
            Ok((spans, Some(format!("matched {}{places}", how.describe()))))
        }
        Err(LooseError::NotFound) => Err(Miss::NotFound),
        Err(LooseError::Ambiguous(how, lines)) => Err(Miss::Ambiguous(
            format!(
                "`old` isn't in the file exactly, and matched {} it fits {} places (lines {:?}). \
                 Add surrounding lines to make it unique, or set `all`.",
                how.describe().split(';').next().unwrap_or(""),
                lines.len(),
                lines.iter().take(8).collect::<Vec<_>>()
            ),
            lines.len(),
        )),
        Err(LooseError::TooLarge { span, want }) => Err(Miss::Refused(format!(
            "`old` only matches loosely across {span} bytes, far more than its own {want}; \
             refusing. Copy the text exactly from a fresh read, or use `lines`."
        ))),
    }
}

/// The 1-based line where `r.new` already stands (and `r.old` doesn't):
/// the change looks applied already. Only for a `new` long enough that
/// finding it means something.
fn already_there(text: &str, r: &Repl) -> Option<usize> {
    if r.to.chars().filter(|c| !c.is_whitespace()).count() < 12 || r.to.contains(&r.from) {
        return None;
    }
    let at = match text.find(&r.to) {
        Some(pos) => pos,
        None => match loose_spans(text, &r.to, &r.to, true) {
            Ok((spans, _)) => spans.first()?.0,
            Err(_) => return None,
        },
    };
    Some(text[..at].matches('\n').count() + 1)
}

/// What a `replace` edit did.
#[derive(Debug)]
pub(crate) struct Replaced {
    pub text: String,
    pub notes: Vec<String>,
    /// Items whose change is in the file already ("replacement 2 (line 40)").
    pub already: Vec<String>,
}

#[cfg(test)]
fn apply_replacements(old: &str, items: &[Value]) -> Result<(String, Vec<String>), String> {
    apply_replacements_ext(old, items)
        .map(|r| (r.text, r.notes))
        .map_err(|e| e.message)
}

/// Why a `replace` edit failed; `all_of`: the item (and how many places it
/// matched) that `all: true` would apply.
#[derive(Debug)]
pub(crate) struct ReplaceError {
    pub message: String,
    pub all_of: Option<(usize, usize)>,
}

impl ReplaceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            all_of: None,
        }
    }
}

impl From<String> for ReplaceError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// The call that would have worked: the model's own, with `all` set on the
/// item that matched several places. `None` when it would be too long to
/// repeat back.
fn with_all(args: &Value, item: usize) -> Option<Value> {
    let mut items = args.get("replace")?.as_array()?.clone();
    items
        .get_mut(item)?
        .as_object_mut()?
        .insert("all".into(), Value::Bool(true));
    let fix = json!({ "path": args.get("path")?, "replace": items });
    (fix.to_string().len() <= 600).then_some(fix)
}

/// Apply `replace` items. Each is first placed in the file as it was, so
/// the order they come in doesn't matter (one item's new text can't create
/// or hide another's match); an item that only matches text an earlier one
/// wrote (or overlaps one) is then applied in order, on the result.
pub(crate) fn apply_replacements_ext(old: &str, items: &[Value]) -> Result<Replaced, ReplaceError> {
    let crlf = old.contains("\r\n");
    let original = if crlf {
        old.replace("\r\n", "\n")
    } else {
        old.to_string()
    };
    let mut repls = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let from = item
            .get("old")
            .and_then(Value::as_str)
            .ok_or(format!("replacement {} is missing `old`", i + 1))?
            .replace("\r\n", "\n");
        let to = item
            .get("new")
            .and_then(Value::as_str)
            .ok_or(format!("replacement {} is missing `new`", i + 1))?
            .replace("\r\n", "\n");
        let all = item.get("all").and_then(Value::as_bool).unwrap_or(false);
        if from.is_empty() {
            return Err(format!(
                "replacement {}: `old` is empty; use `lines` with \">$\" or \"<1\" to insert",
                i + 1
            )
            .into());
        }
        repls.push(Repl { from, to, all });
    }
    let mut notes = Vec::new();
    let mut already = Vec::new();
    // Placed against the file as it was.
    let mut taken: Vec<Span> = Vec::new();
    let mut later = Vec::new();
    for (i, r) in repls.iter().enumerate() {
        let Ok((spans, note)) = place(&original, r) else {
            later.push(i);
            continue;
        };
        if spans.iter().all(|s| taken.contains(s)) {
            notes.push(format!(
                "(Replacement {} repeats an earlier one; applied once.)",
                i + 1
            ));
            continue;
        }
        if spans
            .iter()
            .any(|s| taken.iter().any(|t| s.0 < t.1 && t.0 < s.1))
        {
            later.push(i);
            continue;
        }
        taken.extend(spans);
        if let Some(n) = note {
            notes.push(format!("(Replacement {} {n}.)", i + 1));
        }
    }
    taken.sort_by_key(|t| t.0);
    let mut text = original;
    for (s, e, to) in taken.iter().rev() {
        text.replace_range(*s..*e, to);
    }
    // The rest in order, on the text so far (an item may target what an
    // earlier one wrote, as with MultiEdit).
    for i in later {
        let r = &repls[i];
        match place(&text, r) {
            Ok((spans, note)) => {
                for (s, e, to) in spans.iter().rev() {
                    text.replace_range(*s..*e, to);
                }
                if let Some(n) = note {
                    notes.push(format!("(Replacement {} {n}.)", i + 1));
                }
            }
            Err(Miss::Refused(why)) => {
                return Err(ReplaceError::new(format!("replacement {}: {why}", i + 1)));
            }
            Err(Miss::Ambiguous(why, count)) => {
                return Err(ReplaceError {
                    message: format!("replacement {}: {why}", i + 1),
                    all_of: Some((i, count)),
                });
            }
            Err(Miss::NotFound) => {
                if let Some(line) = already_there(&text, r) {
                    already.push(format!("replacement {} (line {line})", i + 1));
                    notes.push(format!(
                        "(Replacement {}: `old` isn't in the file, but its `new` text already is, at line {line}; left as it is.)",
                        i + 1
                    ));
                    continue;
                }
                return Err(ReplaceError::new(format!(
                    "replacement {}: `old` text not found.{}",
                    i + 1,
                    closest_hint(&text, &r.from)
                )));
            }
        }
    }
    Ok(Replaced {
        text: if crlf {
            text.replace('\n', "\r\n")
        } else {
            text
        },
        notes,
        already,
    })
}

#[derive(Debug, PartialEq)]
pub(crate) enum LooseError {
    NotFound,
    /// 1-based starting lines of the candidates.
    Ambiguous(Loose, Vec<usize>),
    TooLarge {
        span: usize,
        want: usize,
    },
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn leading_ws(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

/// Longest whitespace prefix shared by the non-blank lines.
pub(crate) fn common_indent<'a>(lines: &[&'a str]) -> &'a str {
    let mut it = lines.iter().filter(|l| !is_blank(l)).map(|l| leading_ws(l));
    let Some(first) = it.next() else { return "" };
    let mut len = first.len();
    for w in it {
        len = first
            .bytes()
            .zip(w.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    &first[..len]
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn block_fits(how: Loose, hay: &[&str], needle: &[&str], n_indent: &str) -> bool {
    match how {
        Loose::Indent => {
            let h_indent = common_indent(hay);
            hay.iter().zip(needle).all(|(h, n)| {
                if is_blank(h) || is_blank(n) {
                    return is_blank(h) && is_blank(n);
                }
                match (h.strip_prefix(h_indent), n.strip_prefix(n_indent)) {
                    (Some(hr), Some(nr)) => hr.trim_end() == nr.trim_end(),
                    _ => false,
                }
            })
        }
        Loose::Trimmed => hay
            .iter()
            .zip(needle)
            .all(|(h, n)| collapse(h) == collapse(n)),
    }
}

/// Shift `text`'s indentation from `from` to `to` on every non-blank line
/// that starts with `from`.
pub(crate) fn reindent(text: &str, from: &str, to: &str) -> String {
    if from == to {
        return text.to_string();
    }
    text.split('\n')
        .map(|l| match l.strip_prefix(from) {
            Some(rest) if !is_blank(l) => format!("{to}{rest}"),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The loose strategies, in order; the first that matches decides. Returns
/// the new text, the strategy and how many places were replaced.
#[cfg(test)]
fn loose_replace(
    hay: &str,
    needle: &str,
    to: &str,
    all: bool,
) -> Result<(String, Loose, usize), LooseError> {
    let (spans, how) = loose_spans(hay, needle, to, all)?;
    let mut out = hay.to_string();
    for (s, e, t) in spans.iter().rev() {
        out.replace_range(*s..*e, t);
    }
    Ok((out, how, spans.len()))
}

/// Where the loose strategies place `needle` in `hay`: byte spans with the
/// replacement re-indented for each, and the strategy that matched.
pub(crate) fn loose_spans(
    hay: &str,
    needle: &str,
    to: &str,
    all: bool,
) -> Result<(Vec<Span>, Loose), LooseError> {
    // Too little text to trust a match that ignores spacing inside lines.
    let trivial = needle.chars().filter(|c| !c.is_whitespace()).count() < 3;
    let with_nl = needle.ends_with('\n');
    let nlines: Vec<&str> = needle
        .strip_suffix('\n')
        .unwrap_or(needle)
        .split('\n')
        .collect();
    let mut starts = Vec::new();
    let mut pos = 0;
    let hlines: Vec<&str> = hay
        .split_inclusive('\n')
        .map(|l| {
            starts.push(pos);
            pos += l.len();
            l.strip_suffix('\n').unwrap_or(l)
        })
        .collect();
    let n = nlines.len();
    if hlines.len() < n {
        return Err(LooseError::NotFound);
    }
    let n_indent = common_indent(&nlines);
    for how in [Loose::Indent, Loose::Trimmed] {
        if trivial && how == Loose::Trimmed {
            continue;
        }
        let mut found = Vec::new();
        let mut i = 0;
        while i + n <= hlines.len() {
            if block_fits(how, &hlines[i..i + n], &nlines, n_indent) {
                found.push(i);
                i += n;
            } else {
                i += 1;
            }
        }
        if found.is_empty() {
            continue;
        }
        if found.len() > 1 && !all {
            return Err(LooseError::Ambiguous(
                how,
                found.iter().map(|i| i + 1).collect(),
            ));
        }
        let mut spans = Vec::with_capacity(found.len());
        for &i in &found {
            let s = starts[i];
            let last = i + n - 1;
            let mut e = starts[last] + hlines[last].len();
            if with_nl && e < hay.len() {
                e += 1; // the newline
            }
            let limit = needle.len() * 3 / 2 + 200;
            if e - s > limit {
                return Err(LooseError::TooLarge {
                    span: e - s,
                    want: needle.len(),
                });
            }
            let h_indent = common_indent(&hlines[i..i + n]);
            spans.push((s, e, reindent(to, n_indent, h_indent)));
        }
        return Ok((spans, how));
    }
    Err(LooseError::NotFound)
}

/// [`closest_hint`] for other edit forms (`apply_patch` hunks).
pub(crate) fn closest_hint_for(hay: &str, needle: &str) -> String {
    closest_hint(hay, needle)
}

/// Lines of the closest block shown at most.
const CLOSEST_LINES: usize = 12;

/// Where `old` didn't match: the block of the file most like it (as many
/// lines as `old`), with its line numbers, how similar it is, and the lines
/// themselves (capped), so the retry can copy them exactly.
fn closest_hint(hay: &str, needle: &str) -> String {
    const GENERIC: &str = " Re-read the file and copy the text exactly, or use `lines`.";
    let nlines: Vec<&str> = needle.trim_matches('\n').split('\n').collect();
    let hlines: Vec<&str> = hay.lines().collect();
    let n = nlines.len().max(1);
    if hlines.is_empty() || nlines.iter().all(|l| l.trim().is_empty()) {
        return GENERIC.into();
    }
    let Some((start, score)) = closest_block(&hlines, &nlines) else {
        return " Nothing in the file is close to it. Re-read the file and copy the text exactly, or use `lines`.".into();
    };
    let end = (start + n).min(hlines.len());
    let mut shown: String = (start..end.min(start + CLOSEST_LINES))
        .map(|k| format!("{}:{}\n", k + 1, clip_line(hlines[k])))
        .collect();
    if end - start > CLOSEST_LINES {
        shown.push_str(&format!("… ({} more lines)\n", end - start - CLOSEST_LINES));
    }
    let range = if end - start > 1 {
        format!("lines {}-{}", start + 1, end)
    } else {
        format!("line {}", start + 1)
    };
    format!(
        " The closest block is {range} ({}% similar):\n{shown}Copy the text exactly from there (without the line numbers) and retry.",
        (score * 100.0).round() as u32
    )
}

/// The start (0-based) of the window of `nlines.len()` lines most like
/// `nlines`, and its similarity (0–1), when at least half alike. Candidates
/// start where a line of the needle appears (spacing aside) or where the
/// lines most like its first line are; only those are scored in full.
fn closest_block(hlines: &[&str], nlines: &[&str]) -> Option<(usize, f64)> {
    use std::collections::HashMap;
    let n = nlines.len();
    let max_start = hlines.len().saturating_sub(n);
    let key = |l: &str| collapse(l);
    let mut at: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, l) in hlines.iter().enumerate() {
        let k = key(l);
        if k.len() >= 3 {
            at.entry(k).or_default().push(i);
        }
    }
    // Pre-score: needle lines found at their place in the window.
    let mut cands: HashMap<usize, usize> = HashMap::new();
    for (j, l) in nlines.iter().enumerate() {
        if let Some(pos) = at.get(&key(l)) {
            for &p in pos.iter().take(50) {
                if p >= j {
                    *cands.entry((p - j).min(max_start)).or_default() += 1;
                }
            }
        }
    }
    // The lines most like the needle's first non-blank line.
    if let Some((j, first)) = nlines
        .iter()
        .enumerate()
        .find(|(_, l)| !l.trim().is_empty())
    {
        let first = first.trim();
        let mut best: Vec<(usize, f64)> = hlines
            .iter()
            .enumerate()
            .take(20_000)
            .filter(|(_, l)| {
                let t = l.trim().len();
                t * 2 >= first.len() && t <= first.len() * 2 + 8
            })
            .map(|(i, l)| {
                (
                    i,
                    similar::TextDiff::from_chars(l.trim(), first).ratio() as f64,
                )
            })
            .collect();
        best.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (i, r) in best.into_iter().take(3) {
            if r > 0.5 && i >= j {
                cands.entry((i - j).min(max_start)).or_default();
            }
        }
    }
    let mut ranked: Vec<(usize, usize)> = cands.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let want: String = nlines.iter().map(|l| key(l)).collect::<Vec<_>>().join("\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
    let mut best: Option<(usize, f64)> = None;
    for (start, _) in ranked.into_iter().take(12) {
        let end = (start + n).min(hlines.len());
        let have: String = hlines[start..end]
            .iter()
            .map(|l| key(l))
            .collect::<Vec<_>>()
            .join("\n");
        let score = similar::TextDiff::configure()
            .deadline(deadline)
            .diff_chars(&have, &want)
            .ratio() as f64;
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((start, score));
        }
    }
    best.filter(|(_, s)| *s >= 0.5)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn previews_redact_before_a_long_line_is_cut() {
        let secret = "fixture-value-123456";
        let old = format!("{}{}\nold\n", "a".repeat(290), secret);
        let new = old.replace("\nold\n", "\nnew\n");
        let preview = change_preview(&old, &new, &[secret.into()]).0;
        assert!(!preview.contains(&secret[..10]));
        assert!(preview.contains("new"));
        let old = format!("{}{}\nold\n", "a".repeat(199_960), secret);
        let new = old.replace("\nold\n", "\nnew\n");
        let diff = diff_stats_redacted(&old, &new, "notes.txt", &[secret.into()]).2;
        assert!(!diff.contains(&secret[..10]));
    }
}
