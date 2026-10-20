//! web_fetch / web_search.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Value, json};

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, arg_str, opt_str, opt_u64};

/// Search providers that need an API key (stored with the provider
/// credentials under [`key_id`], never shown to models).
pub const KEYED: [&str; 2] = ["brave", "tavily"];

/// Credential id of a search provider's API key.
pub fn key_id(provider: &str) -> String {
    format!("web:{provider}")
}

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 Tessera";
const MAX_BODY: usize = 10 * 1024 * 1024;

/// Addresses that aren't the public internet: this machine, the LAN, link
/// local (cloud metadata services live there), carrier NAT, and the like.
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || a >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] == 0x64 && seg[1] == 0xff9b)
                || (seg[0] == 0x2001 && seg[1] == 0xdb8)
        }
    }
}

/// A URL that names a local or private destination outright.
pub fn internal_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => is_internal(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_internal(IpAddr::V6(ip)),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            !d.contains('.')
                || [
                    ".localhost",
                    ".local",
                    ".lan",
                    ".internal",
                    ".home.arpa",
                    ".intranet",
                ]
                .iter()
                .any(|s| d.ends_with(s))
        }
        None => true,
    }
}

/// Resolves only to public addresses, so a public-looking name can't be
/// pointed (or rebound) at the LAN or this machine.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| !is_internal(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} resolves to a local or private address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Client for URLs that name a public host: it never connects to an
/// internal address, whether through DNS or a redirect.
fn public_client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicOnly))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if internal_url(attempt.url()) {
                    attempt.error("redirect to a local or private address")
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

pub struct WebFetch;

impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> String {
        "Fetch readable text. Page: offset/max_chars (default 30000). prompt: helper answer if available."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "prompt": {"type": "string"},
                "offset": {"type": "integer"},
                "max_chars": {"type": "integer"}
            },
            "required": ["url"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn assess(&self, args: &Value, _ctx: &ToolCtx) -> Risk {
        fetch_risk(args.get("url").and_then(Value::as_str).unwrap_or(""))
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let url = match arg_str(&args, "url") {
                Ok(u) => u.trim().to_string(),
                Err(e) => return e,
            };
            let parsed = match url::Url::parse(&url) {
                Ok(u) if matches!(u.scheme(), "http" | "https") => u,
                _ => return ToolOutput::error("Only http(s) URLs are supported."),
            };
            let Page {
                url: final_url,
                status,
                text,
            } = match fetch_page(ctx, parsed).await {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(e),
            };
            let mut note = String::new();
            if let Some(q) = opt_str(&args, "prompt")
                .map(str::trim)
                .filter(|q| !q.is_empty())
                && (200..300).contains(&status)
                && text.len() > EXTRACT_MIN
                && opt_u64(&args, "offset").is_none()
            {
                match extract(ctx, &final_url, &text, q).await {
                    Ok(out) => return ToolOutput::text(out),
                    Err(e) if ctx.cancel.is_cancelled() => return ToolOutput::error(e),
                    Err(e) => note = format!("[Extract unavailable: {e}. The page follows.]\n"),
                }
            }
            let offset = opt_u64(&args, "offset").unwrap_or(0) as usize;
            // Page to the window's output budget, including the header and
            // continuation hint. Otherwise fit_output cuts it a second time
            // and loses the middle (and sometimes the paging instructions).
            let overhead = note.len() + final_url.len() + 160;
            let max = (opt_u64(&args, "max_chars")
                .unwrap_or(30_000)
                .clamp(1000, 200_000) as usize)
                .min(
                    ctx.host
                        .tool_output_limit(&ctx.session_id)
                        .saturating_sub(overhead),
                );
            let start = crate::util::ceil_char(&text, offset.min(text.len()));
            let end = crate::util::floor_char(&text, start.saturating_add(max).min(text.len()));
            let mut out = format!("{note}{final_url} (HTTP {status})\n\n{}", &text[start..end]);
            if end < text.len() {
                out.push_str(&format!(
                    "\n\n[… {} more characters. Continue with offset={end}]",
                    text.len() - end
                ));
            }
            ToolOutput {
                is_error: !(200..300).contains(&status),
                ..ToolOutput::text(out)
            }
        })
    }
}

/// The LAN and this machine (routers, admin panels, cloud metadata,
/// Tessera's own port) are the user's call, even in read-only sessions.
pub fn fetch_risk(url: &str) -> Risk {
    match url::Url::parse(url.trim()).ok() {
        Some(u) if internal_url(&u) => Risk::Ask {
            reason: format!(
                "fetches a local or private network address ({})",
                u.host_str().unwrap_or("")
            ),
            key: format!("web:{}", u.host_str().unwrap_or("")),
        },
        _ => Risk::None,
    }
}

/// A page as the agent reads it.
pub struct Page {
    /// Where it ended up (after redirects).
    pub url: String,
    pub status: u16,
    /// Readable text (HTML → Markdown), hidden characters removed.
    pub text: String,
}

/// Fetch a page for the agent. Public hosts go through a client that never
/// reaches an internal address; an internal URL must have been approved
/// first (see [`fetch_risk`]). At most 10 MB is read.
pub async fn fetch_page(ctx: &ToolCtx, url: url::Url) -> Result<Page, String> {
    let http = if internal_url(&url) {
        ctx.host.http()
    } else {
        public_client().clone()
    };
    let resp = tokio::select! {
        r = http.get(url).header("user-agent", UA).header("accept", "text/html,application/xhtml+xml,text/plain,application/json;q=0.9,*/*;q=0.5").timeout(std::time::Duration::from_secs(30)).send() => r,
        _ = ctx.cancel.cancelled() => return Err("Cancelled.".into()),
    };
    let mut resp = resp.map_err(|e| format!("Fetch failed: {e}"))?;
    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ctype.contains("application/pdf") {
        return Err(
            "This URL is a PDF. Download it with bash (curl -Lo file.pdf …) and extract text with `pdftotext`.".into(),
        );
    }
    // Read at most MAX_BODY: a huge or endless response must not exhaust
    // the server's memory.
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if bytes.len() + c.len() > MAX_BODY {
                    return Err("Response is larger than 10 MB.".into());
                }
                bytes.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(e) => return Err(format!("Fetch failed while reading body: {e}")),
        }
    }
    Ok(Page {
        text: readable(&bytes, &ctype, &final_url),
        url: final_url,
        status,
    })
}

/// A response body as readable text. Invisible characters (tag-character
/// "ASCII smuggling", bidi tricks) carry text a person reading the page
/// never sees, so they are removed.
pub fn readable(body: &[u8], content_type: &str, base: &str) -> String {
    let raw = String::from_utf8_lossy(body);
    let text = if content_type.contains("html") || raw.trim_start().starts_with('<') {
        html_to_text(&raw, base)
    } else {
        raw.to_string()
    };
    crate::scan::strip_hidden(&text).0
}

/// Pages shorter than this are returned whole even with a `prompt`.
const EXTRACT_MIN: usize = 6_000;
/// Characters of page per extraction call, and how many calls at most.
const EXTRACT_CHUNK: usize = 24_000;
const EXTRACT_CHUNKS: usize = 4;

const EXTRACT_SYSTEM: &str = "You extract information from a web page for another AI agent. Answer the question using only \
the page text. Be brief: the direct answer first, then the key supporting facts or short verbatim quotes (numbers, names, \
versions and commands exactly as written). Never add knowledge that is not on the page. If this part of the page does not \
contain the answer, reply exactly: NOT FOUND";

/// Ask the utility model to answer `question` from a long page, so only
/// the answer (not the page) enters the agent's context.
async fn extract(ctx: &ToolCtx, url: &str, text: &str, question: &str) -> Result<String, String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() && chunks.len() < EXTRACT_CHUNKS {
        let mut end = crate::util::floor_char(text, (start + EXTRACT_CHUNK).min(text.len()));
        // Prefer to cut at a line break.
        if end < text.len()
            && let Some(nl) = text[start..end]
                .rfind('\n')
                .filter(|&i| i > EXTRACT_CHUNK / 2)
        {
            end = start + nl + 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    let searched = start;
    let ask = |i: usize, chunk: &str| {
        let part = if chunks.len() > 1 {
            format!(" (part {} of {})", i + 1, chunks.len())
        } else {
            String::new()
        };
        let prompt = format!("Question: {question}\n\nPage: {url}{part}\n<page>\n{chunk}\n</page>");
        ctx.host.utility_complete(
            &ctx.session_id,
            EXTRACT_SYSTEM.into(),
            prompt,
            ctx.cancel.clone(),
        )
    };
    // A hosted model reads the parts side by side; a server at home (often
    // one request at a time) one after the other.
    let replies: Vec<String> = if chunks.len() > 1 && ctx.host.utility_parallel(&ctx.session_id) {
        futures::future::join_all(chunks.iter().enumerate().map(|(i, c)| ask(i, c)))
            .await
            .into_iter()
            .collect::<Result<_, _>>()?
    } else {
        let mut out = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            out.push(ask(i, chunk).await?);
        }
        out
    };
    let answers: Vec<String> = replies
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty() && !a.trim_end_matches('.').eq_ignore_ascii_case("not found"))
        .map(str::to_string)
        .collect();
    let store = ctx.host.store();
    let private = store.is_private(&ctx.session_id);
    let artifact = store
        .put_artifact(Some(&ctx.session_id), "web_fetch", text.as_bytes())
        .ok();
    let body = if answers.is_empty() {
        "The page does not seem to answer this.".to_string()
    } else {
        crate::util::truncate(&answers.join("\n\n"), 6_000)
    };
    let mut out = format!(
        "Extract from {url} for “{question}” (written by a helper model from the page):\n{body}\n\n[Page: {} characters",
        text.len()
    );
    if searched < text.len() {
        out.push_str(&format!("; only the first {searched} were searched"));
    }
    match artifact.filter(|_| !private) {
        Some(id) => out.push_str(&format!(
            "; full text: recall artifact=\"{id}\" with grep or offset]"
        )),
        None => out.push_str("; read it with web_fetch and `offset`, without `prompt`]"),
    }
    Ok(out)
}

fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("web regex")
}

/// A regex compiled on first use only (pages are converted often, and
/// compiling is far slower than matching on weak hardware).
macro_rules! once_re {
    ($pat:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| re($pat))
    }};
}

/// Lightweight HTML → readable text with Markdown-ish structure.
pub fn html_to_text(html: &str, base: &str) -> String {
    static STRIP: OnceLock<Vec<Regex>> = OnceLock::new();
    let strip = STRIP.get_or_init(|| {
        [
            "script", "style", "noscript", "svg", "template", "iframe", "head", "nav", "footer",
            "form",
        ]
        .iter()
        .map(|t| re(&format!(r"(?is)<{t}\b.*?</{t}\s*>")))
        .chain([re(r"(?s)<!--.*?-->")])
        .collect()
    });
    let title = once_re!(r"(?is)<title[^>]*>(.*?)</title>")
        .captures(html)
        .map(|c| decode_entities(c[1].trim()))
        .unwrap_or_default();
    // Prefer the main content region when the page marks one.
    let body = once_re!(r"(?is)<(main|article)\b[^>]*>(.*)</(main|article)>")
        .captures(html)
        .map(|c| c[2].to_string())
        .unwrap_or_else(|| html.to_string());
    let mut s = body;
    for r in strip {
        s = r.replace_all(&s, " ").to_string();
    }
    let base_url = url::Url::parse(base).ok();
    s = once_re!(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#)
        .replace_all(&s, |c: &regex::Captures| {
            let text = once_re!(r"(?s)<[^>]+>")
                .replace_all(&c[2], "")
                .trim()
                .to_string();
            let href = base_url
                .as_ref()
                .and_then(|b| b.join(&c[1]).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| c[1].to_string());
            if text.is_empty() || href.starts_with("javascript:") || href.starts_with('#') {
                text
            } else {
                format!("[{text}]({href})")
            }
        })
        .to_string();
    s = once_re!(r"(?is)<h([1-6])[^>]*>")
        .replace_all(&s, |c: &regex::Captures| {
            format!("\n\n{} ", "#".repeat(c[1].parse().unwrap_or(2)))
        })
        .to_string();
    s = once_re!(r"(?is)<pre[^>]*>")
        .replace_all(&s, "\n```\n")
        .to_string();
    s = once_re!(r"(?is)</pre>")
        .replace_all(&s, "\n```\n")
        .to_string();
    s = once_re!(r"(?is)<li[^>]*>")
        .replace_all(&s, "\n- ")
        .to_string();
    s = once_re!(r"(?is)<(br|hr)\s*/?>")
        .replace_all(&s, "\n")
        .to_string();
    s = once_re!(r"(?is)</(p|div|h[1-6]|ul|ol|table|tr|section|blockquote|dl|dt|dd)>")
        .replace_all(&s, "\n")
        .to_string();
    s = once_re!(r"(?is)<t[dh][^>]*>")
        .replace_all(&s, " | ")
        .to_string();
    s = once_re!(r"(?is)<(strong|b)>(.*?)</(strong|b)>")
        .replace_all(&s, "**$2**")
        .to_string();
    s = once_re!(r"(?is)<code>(.*?)</code>")
        .replace_all(&s, "`$1`")
        .to_string();
    s = once_re!(r"(?s)<[^>]+>").replace_all(&s, "").to_string();
    s = decode_entities(&s);
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines() {
        let line = once_re!(r"[ \t\u{a0}]+")
            .replace_all(line.trim(), " ")
            .to_string();
        if line.is_empty() {
            blank += 1;
            if blank == 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank = 0;
        out.push_str(&line);
        out.push('\n');
    }
    if title.is_empty() {
        out
    } else {
        format!("# {title}\n\n{out}")
    }
}

pub fn decode_entities(s: &str) -> String {
    static NUM: OnceLock<Regex> = OnceLock::new();
    let num = NUM.get_or_init(|| re(r"&#(x[0-9a-fA-F]+|[0-9]+);"));
    let s = num
        .replace_all(s, |c: &regex::Captures| {
            let v = &c[1];
            let code = if let Some(h) = v.strip_prefix('x') {
                u32::from_str_radix(h, 16).ok()
            } else {
                v.parse().ok()
            };
            code.and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_default()
        })
        .to_string();
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&copy;", "©")
        .replace("&amp;", "&")
}

pub struct WebSearch;

impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn description(&self) -> String {
        "Search the web (default 8 results): titles, URLs and snippets; follow up with web_fetch for full pages.".into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "required": ["query"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let query = match arg_str(&args, "query") {
                Ok(q) => q.to_string(),
                Err(e) => return e,
            };
            let limit = opt_u64(&args, "limit").unwrap_or(8).clamp(1, 25) as usize;
            let cfg = ctx.host.web_config();
            let http = ctx.host.http();
            // The configured provider first, then the rest of the chain.
            let order = search_order(&cfg, |p| ctx.host.search_key(p).is_some());
            let mut failures: Vec<String> = Vec::new();
            let mut answered: Option<(String, Hits)> = None;
            for provider in &order {
                if ctx.cancel.is_cancelled() {
                    return ToolOutput::error("Cancelled.");
                }
                let result = match provider.as_str() {
                    "searxng" => match cfg.searxng_url.as_deref() {
                        Some(base) => searxng(&http, base, &ctx.host.search_headers(), &query)
                            .await
                            .map_err(SearchErr::from),
                        None => continue,
                    },
                    "brave" | "tavily" => {
                        let Some(key) = ctx.host.search_key(provider) else {
                            continue;
                        };
                        if provider == "brave" {
                            brave(&http, cfg.brave_url.as_deref(), &key, &query, limit).await
                        } else {
                            tavily(&http, cfg.tavily_url.as_deref(), &key, &query, limit).await
                        }
                    }
                    _ => duckduckgo(&http, &query).await.map_err(|e| SearchErr {
                        cooldown: e.contains("rate-limited").then_some(COOLDOWN),
                        message: e,
                    }),
                };
                match result {
                    Ok(hits) if !hits.is_empty() => {
                        cool_down(provider, None);
                        answered = Some((provider.clone(), hits));
                        break;
                    }
                    Ok(_) => failures.push(format!("{}: no results", provider_name(provider))),
                    Err(e) => {
                        cool_down(provider, e.cooldown);
                        failures.push(format!("{}: {}", provider_name(provider), e.message));
                    }
                }
            }
            match answered {
                Some((provider, r)) => {
                    let r = relevant(&query, r);
                    let body: Vec<String> = r
                        .into_iter()
                        .take(limit)
                        .enumerate()
                        .map(|(i, (title, url, snippet))| {
                            format!("{}. {title}\n   {url}\n   {snippet}", i + 1)
                        })
                        .collect();
                    let mut text = search_heading(&query, &provider) + &body.join("\n\n");
                    if !failures.is_empty() {
                        text = format!("({})\n\n{text}", failures.join("; "));
                    }
                    ToolOutput::text(text)
                }
                None if failures.iter().all(|f| f.ends_with("no results")) => {
                    ToolOutput::text(format!("No results for “{query}”."))
                }
                None => ToolOutput::error(format!("Search failed: {}", failures.join("; "))),
            }
        })
    }
}

fn search_heading(query: &str, provider: &str) -> String {
    format!(
        "Search via {}. Query sent unchanged: {query:?}. Upstream relevance/query handling is not verified; check the sources.\n\n",
        provider_name(provider)
    )
}

type Hits = Vec<(String, String, String)>;

/// A provider's failure; `cooldown` is set when it rate-limited us.
struct SearchErr {
    message: String,
    cooldown: Option<std::time::Duration>,
}

impl From<String> for SearchErr {
    fn from(message: String) -> Self {
        Self {
            message,
            cooldown: None,
        }
    }
}

/// How long a provider that rate-limited us is skipped (unless nothing
/// else is left).
const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

fn cooldowns() -> &'static parking_lot::Mutex<std::collections::HashMap<String, std::time::Instant>>
{
    static C: OnceLock<parking_lot::Mutex<std::collections::HashMap<String, std::time::Instant>>> =
        OnceLock::new();
    C.get_or_init(Default::default)
}

fn cool_down(provider: &str, wait: Option<std::time::Duration>) {
    let mut c = cooldowns().lock();
    match wait {
        Some(w) => {
            c.insert(
                provider.to_string(),
                std::time::Instant::now() + w.min(COOLDOWN * 10),
            );
        }
        None => {
            c.remove(provider);
        }
    }
}

fn provider_name(p: &str) -> &str {
    match p {
        "searxng" => "SearXNG",
        "brave" => "Brave",
        "tavily" => "Tavily",
        "duckduckgo" => "DuckDuckGo",
        other => other,
    }
}

/// Providers to try, in order: `search` first (unless `auto`), then the
/// chain; unconfigured ones are dropped and rate-limited ones go last.
fn search_order(cfg: &crate::config::WebConfig, has_key: impl Fn(&str) -> bool) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let first = cfg.search.trim().to_ascii_lowercase();
    for p in std::iter::once(first).chain(cfg.chain.iter().map(|p| p.trim().to_ascii_lowercase())) {
        if !p.is_empty() && p != "auto" && !order.contains(&p) {
            order.push(p);
        }
    }
    order.retain(|p| match p.as_str() {
        "searxng" => cfg.searxng_url.is_some(),
        "brave" | "tavily" => has_key(p),
        "duckduckgo" => true,
        _ => false,
    });
    if order.is_empty() {
        order.push("duckduckgo".into());
    }
    let now = std::time::Instant::now();
    let cooling = cooldowns().lock();
    order.sort_by_key(|p| cooling.get(p).is_some_and(|t| *t > now));
    order
}

fn rate_limit_wait(resp: &reqwest::Response) -> std::time::Duration {
    ["retry-after", "x-ratelimit-reset"]
        .iter()
        .find_map(|h| {
            resp.headers()
                .get(*h)?
                .to_str()
                .ok()?
                .split(',')
                .next()?
                .trim()
                .parse::<f64>()
                .ok()
        })
        .map(|s| std::time::Duration::from_secs_f64(s.clamp(1.0, 3600.0)))
        .unwrap_or(COOLDOWN)
}

/// Error for a non-success HTTP status from a search API.
fn http_failure(resp: &reqwest::Response) -> SearchErr {
    let status = resp.status();
    SearchErr {
        message: match status.as_u16() {
            429 => "rate limit reached (HTTP 429)".to_string(),
            401 | 403 => format!("the API key was rejected (HTTP {})", status.as_u16()),
            s => format!("HTTP {s}"),
        },
        cooldown: (status.as_u16() == 429).then(|| rate_limit_wait(resp)),
    }
}

fn plain(s: &str) -> String {
    decode_entities(&once_re!(r"(?s)<[^>]+>").replace_all(s, ""))
        .trim()
        .to_string()
}

/// Brave Search API (`X-Subscription-Token`).
async fn brave(
    http: &reqwest::Client,
    base: Option<&str>,
    key: &str,
    query: &str,
    count: usize,
) -> Result<Hits, SearchErr> {
    let base = base
        .unwrap_or("https://api.search.brave.com")
        .trim_end_matches('/');
    let resp = http
        .get(format!("{base}/res/v1/web/search"))
        .query(&[("q", query), ("count", &count.min(20).to_string())])
        .header("accept", "application/json")
        .header("x-subscription-token", key)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| SearchErr::from(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(http_failure(&resp));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| SearchErr::from(format!("invalid JSON: {e}")))?;
    Ok(v["web"]["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    (
                        plain(r["title"].as_str().unwrap_or("")),
                        r["url"].as_str().unwrap_or("").to_string(),
                        plain(r["description"].as_str().unwrap_or("")),
                    )
                })
                .filter(|(_, u, _)| !u.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

/// Tavily search API (bearer key).
async fn tavily(
    http: &reqwest::Client,
    base: Option<&str>,
    key: &str,
    query: &str,
    count: usize,
) -> Result<Hits, SearchErr> {
    let base = base
        .unwrap_or("https://api.tavily.com")
        .trim_end_matches('/');
    let resp = http
        .post(format!("{base}/search"))
        .bearer_auth(key)
        .json(&json!({"query": query, "max_results": count.min(20), "search_depth": "basic"}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| SearchErr::from(e.to_string()))?;
    if !resp.status().is_success() {
        // Tavily reports an exhausted plan as 432/433.
        let mut e = http_failure(&resp);
        if matches!(resp.status().as_u16(), 432 | 433) {
            e.message = format!("usage limit reached (HTTP {})", resp.status().as_u16());
            e.cooldown = Some(COOLDOWN * 10);
        }
        return Err(e);
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| SearchErr::from(format!("invalid JSON: {e}")))?;
    Ok(v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    (
                        r["title"].as_str().unwrap_or("").trim().to_string(),
                        r["url"].as_str().unwrap_or("").to_string(),
                        crate::util::truncate(r["content"].as_str().unwrap_or("").trim(), 400),
                    )
                })
                .filter(|(_, u, _)| !u.is_empty())
                .collect()
        })
        .unwrap_or_default())
}

async fn duckduckgo(http: &reqwest::Client, query: &str) -> Result<Hits, String> {
    let resp = http
        .post("https://html.duckduckgo.com/html/")
        .header("user-agent", UA)
        .form(&[("q", query), ("kl", "wt-wt")])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let html = resp.text().await.map_err(|e| e.to_string())?;
    if html.contains("anomaly-modal") || html.contains("challenge-form") {
        return Err("rate-limited this server; for reliable search configure SearXNG (web.searxng_url) or a Brave or Tavily API key".into());
    }
    static A: OnceLock<Regex> = OnceLock::new();
    static SNIP: OnceLock<Regex> = OnceLock::new();
    let a =
        A.get_or_init(|| re(r#"(?is)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#));
    let snip = SNIP.get_or_init(|| re(r#"(?is)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#));
    let tag = once_re!(r"(?s)<[^>]+>");
    let titles: Vec<(String, String)> = a
        .captures_iter(&html)
        .map(|c| (c[1].to_string(), tag.replace_all(&c[2], "").to_string()))
        .collect();
    let snippets: Vec<String> = snip
        .captures_iter(&html)
        .map(|c| tag.replace_all(&c[1], "").to_string())
        .collect();
    Ok(titles
        .into_iter()
        .enumerate()
        .filter_map(|(i, (href, title))| {
            let href = decode_entities(&href);
            let real = if href.contains("uddg=") {
                url::Url::parse(&format!("https:{}", href.trim_start_matches("https:")))
                    .ok()
                    .and_then(|u| {
                        u.query_pairs()
                            .find(|(k, _)| k == "uddg")
                            .map(|(_, v)| v.to_string())
                    })
                    .unwrap_or(href)
            } else {
                href
            };
            if real.contains("duckduckgo.com/y.js") {
                return None; // ads
            }
            Some((
                decode_entities(title.trim()),
                real,
                decode_entities(snippets.get(i).map(|s| s.trim()).unwrap_or("")),
            ))
        })
        .collect())
}

/// Drop results that share nothing with the query (metasearch engines pad
/// with unrelated "latest news"), and duplicates. Order is kept.
fn relevant(query: &str, hits: Hits) -> Hits {
    const STOP: [&str; 24] = [
        "the", "and", "for", "with", "what", "when", "where", "which", "who", "how", "why",
        "latest", "new", "newest", "best", "top", "current", "today", "now", "does", "are", "is",
        "was", "about",
    ];
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|t| t.len() >= 3 && !STOP.contains(&t.as_str()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let filtered: Hits = hits
        .iter()
        .filter(|(_, url, _)| {
            seen.insert(
                url.split(['?', '#'])
                    .next()
                    .unwrap_or(url)
                    .trim_end_matches('/')
                    .to_string(),
            )
        })
        .filter(|(title, url, snippet)| {
            let hay = format!("{title} {url} {snippet}").to_lowercase();
            terms.is_empty() || terms.iter().any(|t| hay.contains(t.as_str()))
        })
        .cloned()
        .collect();
    // Better some results than none.
    if filtered.is_empty() { hits } else { filtered }
}

async fn searxng(
    http: &reqwest::Client,
    base: &str,
    headers: &[(String, String)],
    query: &str,
) -> Result<Hits, String> {
    let mut rb = http
        .get(format!("{}/search", base.trim_end_matches('/')))
        .query(&[("q", query), ("format", "json")])
        .timeout(std::time::Duration::from_secs(20));
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: Value = resp.json().await.map_err(|e| {
        format!("SearXNG did not return JSON (enable the json format in settings.yml): {e}")
    })?;
    Ok(v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    (
                        r["title"].as_str().unwrap_or("").to_string(),
                        r["url"].as_str().unwrap_or("").to_string(),
                        r["content"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results_disclose_backend_and_exact_compound_query() {
        let query = "site:help.openai.com \"Sign in with ChatGPT\" API realtime";
        let heading = search_heading(query, "duckduckgo");
        assert!(heading.contains("DuckDuckGo"), "{heading}");
        assert!(heading.contains(&format!("{query:?}")), "{heading}");
        assert!(
            heading.contains("Upstream relevance/query handling is not verified"),
            "{heading}"
        );
        assert!(search_heading(query, "searxng").contains("SearXNG"));
    }

    #[test]
    fn internal_destinations_are_recognised() {
        for u in [
            "http://127.0.0.1:7878/api/health",
            "http://localhost:3000",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",
            "http://router/",
            "http://nas.lan/",
            "http://[fd00::1]/",
        ] {
            assert!(internal_url(&url::Url::parse(u).unwrap()), "{u}");
        }
        for u in [
            "https://example.com/",
            "http://93.184.216.34/",
            "https://[2606:4700::1]/",
        ] {
            assert!(!internal_url(&url::Url::parse(u).unwrap()), "{u}");
        }
    }

    #[tokio::test]
    async fn public_names_never_resolve_to_internal_addresses() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        assert!(PublicOnly.resolve(name).await.is_err());
        // And the public client refuses to follow a redirect inward.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/admin\r\nContent-Length: 0\r\n\r\n"
            );
            let _ = sock.write_all(reply.as_bytes()).await;
        });
        let err = public_client()
            .get(format!("http://127.0.0.1:{port}/"))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_redirect(), "{err:?}");
    }

    #[test]
    fn search_order_follows_config_keys_and_cooldowns() {
        let mut cfg = crate::config::WebConfig::default();
        let keys = |p: &str| p == "tavily";
        assert_eq!(search_order(&cfg, keys), vec!["tavily", "duckduckgo"]);
        cfg.searxng_url = Some("https://sx.example".into());
        assert_eq!(
            search_order(&cfg, keys),
            vec!["searxng", "tavily", "duckduckgo"]
        );
        cfg.search = "tavily".into();
        assert_eq!(
            search_order(&cfg, keys),
            vec!["tavily", "searxng", "duckduckgo"]
        );
        // Nothing usable: DuckDuckGo still answers.
        cfg.chain = vec!["brave".into()];
        cfg.search = "brave".into();
        assert_eq!(search_order(&cfg, |_| false), vec!["duckduckgo"]);
        // A provider that just rate-limited us goes last; unknown names are dropped.
        cfg = crate::config::WebConfig {
            search: "duckduckgo".into(),
            chain: vec!["nonsense".into(), "searxng".into()],
            searxng_url: Some("https://sx.example".into()),
            ..Default::default()
        };
        assert_eq!(search_order(&cfg, keys), vec!["duckduckgo", "searxng"]);
        cool_down("duckduckgo", Some(std::time::Duration::from_secs(30)));
        assert_eq!(search_order(&cfg, keys), vec!["searxng", "duckduckgo"]);
        cool_down("duckduckgo", None);
    }

    #[test]
    fn html_conversion() {
        let html = r#"<html><head><title>T &amp; Co</title><script>x()</script></head><body><nav>menu</nav>
            <main><h1>Hello</h1><p>Some <b>bold</b> text with <a href="/doc">a link</a>.</p><ul><li>one</li><li>two</li></ul></main></body></html>"#;
        let t = html_to_text(html, "https://example.com/page");
        assert!(t.starts_with("# T & Co"));
        assert!(t.contains("# Hello"));
        assert!(t.contains("**bold**"));
        assert!(t.contains("[a link](https://example.com/doc)"));
        assert!(t.contains("- one"));
        assert!(!t.contains("x()"));
        assert!(!t.contains("menu"));
    }
}
