//! Credentials the user types in a chat: recognised formats are caught by
//! the server before anything else sees the message ([`find_known`]); the
//! rest (a login, a key without a telltale prefix) the agent moves with
//! `save_secret`. Either way the value goes to the secret store (Passwords &
//! keys, or that chat's own when the user wants to be asked first or not to
//! save) and the conversation keeps only its `$NAME`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Value, json};
use tessera_proto::SecretKind;

use super::{Approve, Tool, ToolCtx, ToolKind, ToolOutput, opt_str};
use crate::config::SecretSaving;

/// Credential formats recognisable on sight: pattern, variable name, what it
/// is, the hosts it is for. More specific prefixes come first.
const KNOWN: &[(&str, &str, &str, &[&str])] = &[
    (
        r"sk-ant-[A-Za-z0-9_-]{20,}",
        "ANTHROPIC_API_KEY",
        "Anthropic API key",
        &["api.anthropic.com"],
    ),
    (
        r"sk-or-[A-Za-z0-9_-]{20,}",
        "OPENROUTER_API_KEY",
        "OpenRouter API key",
        &["openrouter.ai"],
    ),
    (
        r"sk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}",
        "OPENAI_API_KEY",
        "OpenAI API key",
        &["api.openai.com"],
    ),
    (
        r"[sr]k_(?:live|test)_[A-Za-z0-9]{20,}",
        "STRIPE_SECRET_KEY",
        "Stripe secret key",
        &["api.stripe.com"],
    ),
    (r"sk-[A-Za-z0-9_-]{20,}", "API_KEY", "API key", &[]),
    (
        r"github_pat_[A-Za-z0-9_]{40,}|gh[pousr]_[A-Za-z0-9]{30,}",
        "GITHUB_TOKEN",
        "GitHub token",
        &["github.com", "api.github.com"],
    ),
    (
        r"glpat-[A-Za-z0-9_-]{20,}",
        "GITLAB_TOKEN",
        "GitLab token",
        &["gitlab.com"],
    ),
    (
        r"xox[abprs]-[A-Za-z0-9-]{10,}",
        "SLACK_TOKEN",
        "Slack token",
        &["slack.com"],
    ),
    (
        r"AIza[0-9A-Za-z_-]{35}",
        "GOOGLE_API_KEY",
        "Google API key",
        &[],
    ),
    (
        r"hf_[A-Za-z0-9]{30,}",
        "HF_TOKEN",
        "Hugging Face token",
        &["huggingface.co"],
    ),
    (
        r"gsk_[A-Za-z0-9]{30,}",
        "GROQ_API_KEY",
        "Groq API key",
        &["api.groq.com"],
    ),
    (
        r"r8_[A-Za-z0-9]{30,}",
        "REPLICATE_API_TOKEN",
        "Replicate token",
        &["api.replicate.com"],
    ),
    (
        r"xai-[A-Za-z0-9]{40,}",
        "XAI_API_KEY",
        "xAI API key",
        &["api.x.ai"],
    ),
    (
        r"pplx-[A-Za-z0-9]{40,}",
        "PERPLEXITY_API_KEY",
        "Perplexity API key",
        &["api.perplexity.ai"],
    ),
    (
        r"npm_[A-Za-z0-9]{36}",
        "NPM_TOKEN",
        "npm token",
        &["registry.npmjs.org"],
    ),
    (
        r"[0-9]{8,10}:AA[A-Za-z0-9_-]{33}",
        "TELEGRAM_BOT_TOKEN",
        "Telegram bot token",
        &["api.telegram.org"],
    ),
    (
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        "PRIVATE_KEY",
        "private key",
        &[],
    ),
];

/// A Tessera device token: it controls this server, so it is never kept.
const DEVICE_TOKEN: &str = r"tsr_[A-Za-z0-9_-]{30,}";

fn known() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        KNOWN
            .iter()
            .map(|(re, ..)| Regex::new(&bounded(re)).expect("secret pattern"))
            .collect()
    })
}

fn bounded(re: &str) -> String {
    if re.starts_with("-----") {
        re.to_string()
    } else {
        format!(r"\b(?:{re})\b")
    }
}

/// Every recognisable token format (with AWS key ids and device tokens),
/// for scrubbing tool output.
pub(crate) fn token_pattern() -> String {
    let mut alts: Vec<&str> = KNOWN
        .iter()
        .map(|(re, ..)| *re)
        .filter(|re| !re.starts_with("-----"))
        .collect();
    alts.extend([DEVICE_TOKEN, r"AKIA[0-9A-Z]{16}", r"ASIA[0-9A-Z]{16}"]);
    format!(r"\b(?:{})\b", alts.join("|"))
}

/// A credential found in a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    /// Suggested variable name; `None` for a device token (removed, not kept).
    pub name: Option<&'static str>,
    pub label: &'static str,
    pub hosts: &'static [&'static str],
}

/// Credentials in `text` in a recognisable format, in order, not
/// overlapping. Documentation placeholders (`…EXAMPLE`, `xxxx…`) are left.
pub fn find_known(text: &str) -> Vec<Found> {
    static DEVICE: OnceLock<Regex> = OnceLock::new();
    let device = DEVICE.get_or_init(|| Regex::new(&bounded(DEVICE_TOKEN)).unwrap());
    let mut out: Vec<Found> = device
        .find_iter(text)
        .map(|m| Found {
            start: m.start(),
            end: m.end(),
            name: None,
            label: "Tessera device token",
            hosts: &[],
        })
        .collect();
    // Claude subscription credentials are removed, never collected in the vault.
    static CLAUDE: OnceLock<Regex> = OnceLock::new();
    let claude =
        CLAUDE.get_or_init(|| Regex::new(r"\bsk-ant-(?:oat|sid)[A-Za-z0-9_-]+\b").unwrap());
    out.extend(claude.find_iter(text).map(|m| Found {
        start: m.start(),
        end: m.end(),
        name: None,
        label: "Claude sign-in token",
        hosts: &[],
    }));
    for (re, (_, name, label, hosts)) in known().iter().zip(KNOWN) {
        for m in re.find_iter(text) {
            let v = m.as_str();
            let placeholder = [
                "EXAMPLE", "example", "xxxxxxxx", "XXXXXXXX", "00000000", "...",
            ]
            .iter()
            .any(|p| v.contains(p));
            if placeholder || out.iter().any(|f| m.start() < f.end && f.start < m.end()) {
                continue;
            }
            out.push(Found {
                start: m.start(),
                end: m.end(),
                name: Some(name),
                label,
                hosts,
            });
        }
    }
    out.sort_by_key(|f| f.start);
    out
}

/// A two-factor secret in a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Code {
    /// A TOTP setup key (an `otpauth://` link, or a base32 key after words
    /// like "2FA key"): its seed, and whom it is for when the text says.
    Setup {
        seed: String,
        issuer: Option<String>,
        account: Option<String>,
    },
    /// A one-time code the user got by text or mail ("my verification
    /// code is 482 913"), as digits.
    OneTime(String),
}

/// A two-factor secret found in a message, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundCode {
    pub start: usize,
    pub end: usize,
    pub code: Code,
}

/// Two-factor secrets in `text`, in order, not overlapping: setup keys
/// anywhere they are unmistakable (`otpauth://` links) or named as such, and
/// one-time codes only where the words around them say so (a bare number
/// is just a number).
pub fn find_codes(text: &str) -> Vec<FoundCode> {
    static RE: OnceLock<[Regex; 4]> = OnceLock::new();
    let [link, key, before, after] = RE.get_or_init(|| {
        [
            Regex::new(r#"otpauth://totp/[^\s<>"'`)\]]+"#).unwrap(),
            Regex::new(r"(?i)\b(?:2fa|two[- ]?factor|mfa|authenticator|totp|otp)\b[^\n]{0,30}?\b(?:key|secret|seed)\b[^\n]{0,24}?\b([a-z2-7]{4}(?:[ -]?[a-z2-7]{4}){3,15})(?:=*)").unwrap(),
            // "verification code is 482913", "2FA: 482 913", "login code from the bank: 4829"
            Regex::new(r"(?i)\b(?:(?:2fa|two[- ]?factor|mfa|one[- ]?time|verification|security|login|log[- ]?in|sign[- ]?in|authentication|auth|confirmation|access)[ \t]+(?:code|pin|passcode|number)\b[^\n0-9]{0,24}?|(?:2fa|mfa|otp|passcode)\b[ \t]*(?:code)?[ \t]*(?:is|:|=|-)?[ \t]*)\b([0-9]{3}[ -][0-9]{3}|[0-9]{4,8})\b").unwrap(),
            // "482913 is your verification code"
            Regex::new(r"(?i)\b([0-9]{4,8})\b[ \t]+is[ \t]+(?:your|the)\b[^\n0-9]{0,30}?\b(?:code|passcode|pin|otp)\b").unwrap(),
        ]
    });
    let mut out: Vec<FoundCode> = Vec::new();
    let mut add = |start: usize, end: usize, code: Code| {
        if !out.iter().any(|f| start < f.end && f.start < end) {
            out.push(FoundCode { start, end, code });
        }
    };
    for m in link.find_iter(text) {
        if let Some(code) = otpauth(m.as_str()) {
            add(m.start(), m.end(), code);
        }
    }
    for c in key.captures_iter(text) {
        let g = c.get(1).expect("the key");
        let seed: String = g
            .as_str()
            .chars()
            .filter(|c| !matches!(c, ' ' | '-'))
            .collect();
        // A key, not a word: one case throughout, a digit in it, and it works as a seed.
        let one_case = seed.chars().all(|c| !c.is_ascii_lowercase())
            || seed.chars().all(|c| !c.is_ascii_uppercase());
        if one_case
            && seed.chars().any(|c| c.is_ascii_digit())
            && crate::auth::totp(&seed, 0).is_some()
        {
            let seed = seed.to_ascii_uppercase();
            add(
                g.start(),
                g.end(),
                Code::Setup {
                    seed,
                    issuer: None,
                    account: None,
                },
            );
        }
    }
    for re in [before, after] {
        for c in re.captures_iter(text) {
            let g = c.get(1).expect("the code");
            let digits: String = g.as_str().chars().filter(char::is_ascii_digit).collect();
            add(g.start(), g.end(), Code::OneTime(digits));
        }
    }
    out.sort_by_key(|f| f.start);
    out
}

/// An `otpauth://totp/Issuer:account?secret=…&issuer=…` link's seed and names.
fn otpauth(link: &str) -> Option<Code> {
    let u = url::Url::parse(link).ok()?;
    let q = |k: &str| {
        u.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let seed = q("secret")?.to_ascii_uppercase();
    crate::auth::totp(&seed, 0)?;
    let label = u.path().trim_start_matches('/');
    let label = unescape(label);
    let (named, account) = match label.split_once(':') {
        Some((i, a)) => (Some(i.trim().to_string()), Some(a.trim().to_string())),
        None => (None, Some(label.trim().to_string())),
    };
    let nonempty = |s: Option<String>| s.filter(|s| !s.is_empty());
    Some(Code::Setup {
        seed,
        issuer: nonempty(q("issuer")).or(nonempty(named)),
        account: nonempty(account),
    })
}

/// `%40` → `@` (a link's label is percent-encoded).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len())
            .then(|| u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok())
            .flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Why a command can't have the secrets it names at `here` that the
/// session may use elsewhere (one sentence each), or `None`.
pub fn elsewhere(
    uses: &crate::auth::SecretUse,
    machines: impl FnOnce() -> Vec<tessera_proto::MachineInfo>,
    here: crate::auth::SecretPlace,
) -> Option<String> {
    use crate::auth::SecretPlace;
    if let Some(reason) = &uses.blocked {
        return Some(reason.clone());
    }
    if uses.elsewhere.is_empty() {
        return None;
    }
    let machines = machines();
    let name_of = |id: &str| machines.iter().find(|m| m.id == id).map(|m| m.name.clone());
    let here_text = match here {
        SecretPlace::Server => "on this server".to_string(),
        SecretPlace::Machine(id) => format!(
            "on {}",
            name_of(id).unwrap_or_else(|| "that machine".into())
        ),
    };
    let lines: Vec<String> = uses
        .elsewhere
        .iter()
        .map(|(name, server, ids)| {
            let mut places: Vec<String> = Vec::new();
            if *server {
                places.push("on this server (`bash`)".into());
            }
            let names: Vec<String> = ids.iter().filter_map(|id| name_of(id)).collect();
            if !names.is_empty() {
                places.push(format!("on {} (`machines` run)", names.join(", ")));
            }
            if places.is_empty() {
                format!(
                    "${name} can't be used anywhere right now; the user can change that in Settings → Privacy & safety → Passwords & keys."
                )
            } else {
                format!(
                    "${name} can't be used {here_text}, only {}{}.",
                    places.join(" or "),
                    match here {
                        SecretPlace::Machine(_) => format!(
                            "; the user can allow it {here_text} in Settings → Privacy & safety → Passwords & keys"
                        ),
                        SecretPlace::Server => String::new(),
                    }
                )
            }
        })
        .collect();
    Some(lines.join(" "))
}

/// Titles that say nothing about what a password is for: what Tessera used
/// to give everything saved from a chat.
fn says_little(title: &str, name: &str) -> bool {
    // The variable name itself ("ROUTER"), not a title written like it ("Router").
    if title.trim() == name {
        return true;
    }
    let t = title.trim().to_ascii_lowercase();
    t.is_empty()
        || matches!(t.as_str(), "saved from a chat" | "typed in a chat")
        || t.ends_with(", saved from a chat")
        || t.ends_with(", typed in a chat")
}

/// Words of a variable name the way people write them.
const SPELLED: &[(&str, &str)] = &[
    ("github", "GitHub"),
    ("gitlab", "GitLab"),
    ("gitea", "Gitea"),
    ("openai", "OpenAI"),
    ("openrouter", "OpenRouter"),
    ("anthropic", "Anthropic"),
    ("cloudflare", "Cloudflare"),
    ("google", "Google"),
    ("telegram", "Telegram"),
    ("signal", "Signal"),
    ("paypal", "PayPal"),
    ("api", "API"),
    ("aws", "AWS"),
    ("nas", "NAS"),
    ("wifi", "Wi-Fi"),
    ("otp", "one-time code"),
    ("pw", "password"),
    ("pwd", "password"),
    ("pass", "password"),
    ("passwd", "password"),
    ("usr", "user"),
    ("db", "database"),
];

/// A clear title for a saved password from what is known about it without
/// its values: its name, kind and sites, and the user's machine names
/// (`DESKTOP_SUDO`, a login → "Desktop sudo login"; `GITHUB_TOKEN` → "GitHub
/// token"; `ROUTER` with `192.168.1.1` → "Router login (192.168.1.1)").
pub fn suggest_title(
    name: &str,
    kind: SecretKind,
    hosts: &[String],
    machines: &[String],
) -> String {
    // `NAME_2`: the second of its kind.
    let (base, nth) = match name.rsplit_once('_') {
        Some((b, n)) if !b.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => (b, Some(n)),
        _ => (name, None),
    };
    let known = KNOWN.iter().find(|(_, var, ..)| *var == base).map(|k| k.2);
    let mut words: Vec<String> = match known {
        Some(label) => vec![label.to_string()],
        None => base
            .split('_')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let lw = w.to_ascii_lowercase();
                if let Some(m) = machines.iter().find(|m| m.eq_ignore_ascii_case(w)) {
                    m.clone()
                } else if let Some((_, s)) = SPELLED.iter().find(|(k, _)| *k == lw) {
                    s.to_string()
                } else if w.len() <= 5 && !lw.contains(['a', 'e', 'i', 'o', 'u', 'y']) {
                    // No vowels: an abbreviation (SMTP, DNS, PC).
                    w.to_ascii_uppercase()
                } else {
                    lw
                }
            })
            .collect(),
    };
    let has = |ws: &[String], any: &[&str]| {
        ws.iter()
            .any(|w| any.iter().any(|a| w.to_ascii_lowercase().contains(a)))
    };
    match kind {
        SecretKind::Token
            if !has(
                &words,
                &["key", "token", "secret", "password", "code", "pin"],
            ) =>
        {
            words.push("key".into())
        }
        SecretKind::Login
            if !has(
                &words,
                &["login", "password", "account", "sign", "credential", "user"],
            ) =>
        {
            words.push("login".into())
        }
        _ => {}
    }
    let mut title = words.join(" ");
    if let Some(first) = title.chars().next()
        && first.is_lowercase()
    {
        title = first.to_uppercase().collect::<String>() + &title[first.len_utf8()..];
    }
    if let Some(n) = nth {
        title.push_str(&format!(" {n}"));
    }
    // The site, unless the title names it already.
    if let Some(host) = hosts.first() {
        let host = host.trim_start_matches("www.");
        // `mybank` of `online.mybank.com`; an address stays whole.
        let address = host
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ':'));
        let main = match host.split('.').rev().nth(1) {
            Some(m) if !address => m,
            _ => host,
        };
        if !title
            .to_ascii_lowercase()
            .contains(&main.to_ascii_lowercase())
        {
            title.push_str(&format!(" ({host})"));
        }
    }
    crate::util::one_line(&title, 80)
}

/// Clearer titles for the saved passwords and keys whose titles say
/// little: the part before ", saved from a chat" where there is one
/// ("GitHub token"), else [`suggest_title`]. Deterministic.
pub fn suggest_titles(
    list: &[tessera_proto::SecretInfo],
    machines: &[String],
) -> Vec<tessera_proto::SecretTitle> {
    list.iter()
        .filter(|s| says_little(&s.description, &s.name))
        .filter_map(|s| {
            let kept = s
                .description
                .trim()
                .trim_end_matches(", saved from a chat")
                .trim_end_matches(", typed in a chat")
                .trim();
            let suggested = if !kept.is_empty() && !says_little(kept, &s.name) {
                kept.to_string()
            } else {
                suggest_title(&s.name, s.kind, &s.hosts, machines)
            };
            (suggested != s.description).then(|| tessera_proto::SecretTitle {
                name: s.name.clone(),
                title: s.description.clone(),
                suggested,
            })
        })
        .collect()
}

/// The title `save_secret` stores: the agent's, unless it is empty, says
/// little or carries one of the values (titles are shown in the apps).
pub fn chat_secret_title(
    given: Option<&str>,
    name: &str,
    kind: SecretKind,
    fields: &BTreeMap<String, String>,
    hosts: &[String],
    machines: &[String],
) -> String {
    let given = given.map(|t| crate::util::one_line(t.trim(), 80));
    let carries_value = |t: &str| {
        let t = t.to_lowercase();
        fields
            .iter()
            .filter(|(f, v)| f.as_str() != "url" && v.chars().count() >= 3)
            .any(|(_, v)| t.contains(&v.to_lowercase()))
    };
    match given {
        Some(t) if !says_little(&t, name) && !carries_value(&t) => t,
        _ => suggest_title(name, kind, hosts, machines),
    }
}

/// What `save_secret` asks the engine to store.
#[derive(Debug, Clone)]
pub struct ChatSecret {
    pub name: String,
    pub kind: SecretKind,
    pub description: String,
    /// `value` for a token; `username`, `password`… for a login.
    pub fields: BTreeMap<String, String>,
    pub hosts: Vec<String>,
    /// Keep it for this chat alone, not in Passwords & keys.
    pub chat_only: bool,
}

const LOGIN_FIELDS: [&str; 3] = ["username", "password", "url"];

/// Value-free request used by Claude's bridge and the secure entry card.
/// Values travel only from the user's app to the vault, never in tool calls.
pub fn entry_schema() -> Value {
    json!({"type": "object", "properties": {
        "action": {"type": "string", "enum": ["list", "request"]},
        "name": {"type": "string"},
        "title": {"type": "string"},
        "kind": {"type": "string", "enum": ["token", "login"]},
        "url": {"type": "string"}
    }, "additionalProperties": false})
}

pub fn claude_credential(name: &str, title: &str, url: &str) -> bool {
    let text = format!("{name} {title} {url}").to_ascii_lowercase();
    text.contains("claude") || text.contains("sk-ant-oat") || text.contains("sk-ant-sid")
}

pub struct SaveSecret;

impl Tool for SaveSecret {
    fn name(&self) -> &'static str {
        "save_secret"
    }
    fn description(&self) -> String {
        "Move a credential the user typed in this chat (a login, or a key Tessera didn't catch) to their secrets; it is \
         then removed from the conversation. Afterwards use only $NAME, or $NAME_USERNAME, $NAME_PASSWORD."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "e.g. GITHUB_TOKEN, BANK_LOGIN"},
                "title": {"type": "string", "description": "what it's for, e.g. Bank login"},
                "value": {"type": "string", "description": "a token or key"},
                "username": {"type": "string"},
                "password": {"type": "string"},
                "url": {"type": "string", "description": "sign-in page"}
            },
            "required": ["name"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn summary(&self, args: &Value) -> String {
        // Never a value: the default summary shows the first string argument.
        if opt_str(args, "action") == Some("list") {
            return "list saved passwords".into();
        }
        opt_str(args, "name").unwrap_or("secret").to_string()
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            // A lean start lists only some of them in the prompt: the rest,
            // as the prompt would list them (names, never values).
            if opt_str(&args, "action") == Some("list") {
                return ToolOutput::text(ctx.host.list_secrets(&ctx.session_id));
            }
            let Some(name) = opt_str(&args, "name").map(|n| n.trim().to_ascii_uppercase()) else {
                return ToolOutput::error("Missing `name`.");
            };
            if !["value", "username", "password"]
                .iter()
                .any(|f| args.get(f).is_some())
            {
                let kind = if opt_str(&args, "kind") == Some("login") {
                    SecretKind::Login
                } else {
                    SecretKind::Token
                };
                let title = opt_str(&args, "title").unwrap_or(&name);
                let url = opt_str(&args, "url").unwrap_or("");
                if claude_credential(&name, title, url) {
                    return ToolOutput::error(
                        "Claude sign-in belongs to the user's own Claude CLI, not Tessera's password library.",
                    );
                }
                let secret = ChatSecret {
                    name: name.clone(),
                    kind,
                    description: title.into(),
                    fields: BTreeMap::new(),
                    hosts: vec![],
                    chat_only: false,
                };
                if let Err(e) = ctx.host.check_chat_secret(&ctx.session_id, &secret) {
                    return ToolOutput::error(e);
                }
                return ToolOutput::text("Not saved yet. The user can enter it in the secure card in Tessera, or in Settings → Passwords & keys. Never ask them to send its value in chat.")
                    .with_details(json!({"secret_entry": {"name": name, "title": title, "kind": kind, "url": url}}));
            }
            let mut fields = BTreeMap::new();
            // A login's site: where the browser may type it.
            let hosts: Vec<String> = opt_str(&args, "url")
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_string))
                .into_iter()
                .collect();
            let kind = if let Some(v) = opt_str(&args, "value") {
                fields.insert("value".to_string(), v.to_string());
                SecretKind::Token
            } else {
                for f in LOGIN_FIELDS {
                    if let Some(v) = opt_str(&args, f) {
                        fields.insert(f.to_string(), v.to_string());
                    }
                }
                SecretKind::Login
            };
            if !fields.contains_key("value")
                && !fields.contains_key("username")
                && !fields.contains_key("password")
            {
                return ToolOutput::error(
                    "Give `value` (a token) or the login's `username` and `password` (and `url`).",
                );
            }
            let description = chat_secret_title(
                opt_str(&args, "title"),
                &name,
                kind,
                &fields,
                &hosts,
                &ctx.host
                    .machines()
                    .into_iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>(),
            );
            let mut secret = ChatSecret {
                name,
                kind,
                description,
                fields,
                hosts,
                chat_only: false,
            };
            let saving = match ctx.host.check_chat_secret(&ctx.session_id, &secret) {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(e),
            };
            let mut approval_note = None;
            secret.chat_only = match saving {
                SecretSaving::Auto => false,
                SecretSaving::ChatOnly => true,
                // The user's call, whatever the permission mode; no: this
                // chat keeps it (the values leave the conversation anyway).
                SecretSaving::Ask => {
                    let what = if secret.kind == SecretKind::Token {
                        "a key"
                    } else {
                        "a login"
                    };
                    match ctx.host
                        .approve_reason(Approve {
                            session_id: ctx.session_id.clone(),
                            call_id: ctx.call_id.clone(),
                            tool: "save_secret".into(),
                            summary: format!("Save {} ({what}) to Passwords & keys", secret.name),
                            reason: "Any chat could then use it, asking you first. If you deny, only this chat keeps it.".into(),
                            key: format!("save_secret:{}", secret.name),
                            always: true,
                            cancel: ctx.cancel.clone(),
                            // Never the values.
                            arguments: json!({}),
                            once: false,
                        })
                        .await {
                            Ok(()) => false,
                            Err(why) => { approval_note = Some(why); true }
                        }
                }
            };
            if ctx.cancel.is_cancelled() {
                return ToolOutput::error("Stopped before it was saved.");
            }
            match ctx.host.save_chat_secret(&ctx.session_id, secret) {
                Ok(text) => ToolOutput::text(match approval_note {
                    Some(why) => format!("{why}\n{text}"),
                    None => text,
                }),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{SecretPlace, SecretUse};

    #[test]
    fn two_factor_keys_and_one_time_codes_are_found_by_their_words() {
        let one = |t: &str| -> Vec<Code> { find_codes(t).into_iter().map(|f| f.code).collect() };
        let otp = |d: &str| Code::OneTime(d.into());
        // One-time codes, with the words that make them one.
        assert_eq!(one("my verification code is 482913"), [otp("482913")]);
        assert_eq!(one("2FA: 482 913"), [otp("482913")]);
        assert_eq!(
            one("the login code from the bank: 4829, thanks"),
            [otp("4829")]
        );
        assert_eq!(one("482913 is your Shop sign-in code"), [otp("482913")]);
        assert_eq!(one("security code 731"), Vec::<Code>::new()); // too short for a code
        // Numbers that aren't codes stay.
        for t in [
            "call me at 482913",
            "error code 500123",
            "the zip code is 73301",
            "2fa is on since 2019",
            "security update 2024",
            "port 8080 and the code is in main.rs",
        ] {
            assert_eq!(one(t), Vec::<Code>::new(), "{t}");
        }
        // A setup key: a link, wherever it is.
        let link = "otpauth://totp/Shop:ann%40example.com?secret=gezdgnbvgy3tqojqgezdgnbvgy3tqojq&issuer=Shop&digits=6";
        let f = find_codes(&format!("scan this: {link} ok"));
        assert_eq!(
            (f.len(), &f[0].code),
            (
                1,
                &Code::Setup {
                    seed: "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".into(),
                    issuer: Some("Shop".into()),
                    account: Some("ann@example.com".into()),
                }
            )
        );
        assert_eq!(&format!("scan this: {link} ok")[f[0].start..f[0].end], link);
        // Or named as one, in groups or not; words after "2fa key" are not a key.
        assert_eq!(
            one("my 2fa setup key for the shop: gezd gnbv gy3t qojq"),
            [Code::Setup {
                seed: "GEZDGNBVGY3TQOJQ".into(),
                issuer: None,
                account: None
            }]
        );
        assert_eq!(
            one("2FA secret JBSWY3DPEHPK3PXP"),
            [Code::Setup {
                seed: "JBSWY3DPEHPK3PXP".into(),
                issuer: None,
                account: None
            }]
        );
        assert_eq!(
            one("the 2fa key isnt working anymore, authenticationfailed"),
            Vec::<Code>::new()
        );
    }

    #[test]
    fn a_refusal_says_where_a_secret_does_work() {
        let machine = |id: &str, name: &str| tessera_proto::MachineInfo {
            id: id.into(),
            name: name.into(),
            ..serde_json::from_value(json!({
                "id": "", "name": "", "os": "linux", "version": "0", "online": true, "last_seen": 0,
                "paused": false, "scope": {}, "sandbox": "on", "sandbox_ok": true, "running": 0
            }))
            .unwrap()
        };
        let ms = vec![machine("m1", "Desktop"), machine("m2", "nas")];
        let uses = SecretUse {
            elsewhere: vec![
                ("LAB".into(), false, vec!["m1".into()]),
                ("ANY".into(), true, vec![]),
                ("GONE".into(), false, vec!["old".into()]),
            ],
            ..Default::default()
        };
        assert_eq!(
            elsewhere(
                &SecretUse::default(),
                || unreachable!(),
                SecretPlace::Server
            ),
            None
        );
        let here = elsewhere(&uses, || ms.clone(), SecretPlace::Server).unwrap();
        assert!(
            here.contains("$LAB can't be used on this server, only on Desktop (`machines` run)."),
            "{here}"
        );
        assert!(
            here.contains("$GONE can't be used anywhere right now"),
            "{here}"
        );
        let there = elsewhere(&uses, || ms.clone(), SecretPlace::Machine("m2")).unwrap();
        assert!(
            there.contains(
                "$ANY can't be used on nas, only on this server (`bash`); the user can allow it on nas in Settings → Privacy & safety → Passwords & keys."
            ),
            "{there}"
        );
    }

    #[test]
    fn titles_say_what_a_password_is_for_without_its_values() {
        let pc = vec!["Desktop".to_string()];
        let t = |name: &str, kind: SecretKind, hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
            suggest_title(name, kind, &hosts, &pc)
        };
        assert_eq!(
            t("DESKTOP_SUDO", SecretKind::Login, &[]),
            "Desktop sudo login"
        );
        assert_eq!(
            t("GITHUB_TOKEN", SecretKind::Token, &["github.com"]),
            "GitHub token"
        );
        assert_eq!(t("API_KEY_2", SecretKind::Token, &[]), "API key 2");
        assert_eq!(
            t("ROUTER", SecretKind::Login, &["192.168.1.1"]),
            "Router login (192.168.1.1)"
        );
        assert_eq!(
            t("BANK_LOGIN", SecretKind::Login, &["www.mybank.com"]),
            "Bank login (mybank.com)"
        );
        assert_eq!(t("SMTP_PASS", SecretKind::Custom, &[]), "SMTP password");
        assert_eq!(t("WEATHER", SecretKind::Token, &[]), "Weather key");
        // Which ones say little, and what they become.
        let info =
            |name: &str, title: &str, kind: SecretKind, hosts: &[&str]| tessera_proto::SecretInfo {
                name: name.into(),
                description: title.into(),
                kind,
                hosts: hosts.iter().map(|h| h.to_string()).collect(),
                ..serde_json::from_value(json!({"name": "", "updated_at": 0})).unwrap()
            };
        let list = vec![
            info("DESKTOP_SUDO", "saved from a chat", SecretKind::Login, &[]),
            info(
                "GITHUB_TOKEN",
                "GitHub token, saved from a chat",
                SecretKind::Token,
                &[],
            ),
            info("OPENAI_API_KEY", "", SecretKind::Token, &[]),
            info("ROUTER", "ROUTER", SecretKind::Login, &["192.168.1.1"]),
            info("SHOP", "My shop (EU)", SecretKind::Login, &[]),
            info("GITHUB", "GitHub", SecretKind::Login, &[]),
        ];
        let got: Vec<(String, String)> = suggest_titles(&list, &pc)
            .into_iter()
            .map(|s| (s.name, s.suggested))
            .collect();
        assert_eq!(
            got,
            [
                ("DESKTOP_SUDO".to_string(), "Desktop sudo login".to_string()),
                ("GITHUB_TOKEN".into(), "GitHub token".into()),
                ("OPENAI_API_KEY".into(), "OpenAI API key".into()),
                ("ROUTER".into(), "Router login (192.168.1.1)".into()),
            ]
        );
        // The agent's title, unless it says little or holds a value.
        let fields = BTreeMap::from([
            ("username".to_string(), "ann.smith".to_string()),
            ("password".to_string(), "testpw2-long".to_string()),
        ]);
        let title = |given: Option<&str>| {
            chat_secret_title(given, "BANK", SecretKind::Login, &fields, &[], &pc)
        };
        assert_eq!(title(Some("  Bank (savings)  ")), "Bank (savings)");
        assert_eq!(title(Some("Bank for ann.smith")), "Bank login");
        assert_eq!(title(Some("pw TESTPW2-LONG")), "Bank login");
        assert_eq!(title(Some("saved from a chat")), "Bank login");
        assert_eq!(title(None), "Bank login");
    }

    #[test]
    fn known_formats_are_found_and_named() {
        let text = "anthropic sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345, \
                    gh ghp_abcdefghijklmnopqrstuvwxyz0123456789 and \
                    a device tsr_abcdefghijklmnopqrstuvwxyz0123456789 \
                    plus docs AKIAIOSFODNN7EXAMPLE and sk-proj-xxxxxxxxxxxxxxxxxxxxxxxxxxxx.";
        let found = find_known(text);
        let names: Vec<Option<&str>> = found.iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            [Some("ANTHROPIC_API_KEY"), Some("GITHUB_TOKEN"), None]
        );
        assert!(text[found[0].start..found[0].end].starts_with("sk-ant-api03"));
        assert!(text[found[0].start..found[0].end].ends_with("012345"));
        assert_eq!(found[1].hosts, ["github.com", "api.github.com"]);
        // Prose, code and short look-alikes are left alone.
        for t in [
            "the sk- prefix marks keys",
            "ask-anything-you-like-today-please-now",
            "task_abcdefghijklmnopqrstuvwxyz0123456789",
            "commit 1234567890abcdef",
        ] {
            assert!(find_known(t).is_empty(), "{t}");
        }
        let pem = "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\nthanks";
        let f = find_known(pem);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, Some("PRIVATE_KEY"));
        assert!(pem[f[0].end..].starts_with("\nthanks"));
    }
}
