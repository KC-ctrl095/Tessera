//! `share_file`: put a file from the server into the chat as a download.
//!
//! The same path rules as `read` (Tessera's own store is off limits), and
//! never anything that holds a credential: files named like key or secret
//! stores are refused, and the copy is scanned as it is made for saved
//! secrets, private keys and access tokens.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput};

pub struct ShareFile;

/// The largest file that can be shared.
pub const MAX_SHARE: u64 = 1 << 30;

/// File names that hold credentials, whatever their content looks like.
pub fn secret_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let env = name == ".env"
        || (name.starts_with(".env.")
            && !matches!(ext, "example" | "sample" | "template" | "dist" | "defaults"));
    env || super::credential_file(path)
        || matches!(
            ext,
            "pem" | "key" | "p12" | "pfx" | "jks" | "keystore" | "kdbx" | "ppk" | "asc"
        )
        || (name.starts_with("id_") && !name.ends_with(".pub") && !name.contains('.'))
        || matches!(
            name.as_str(),
            ".npmrc"
                | ".pypirc"
                | ".netrc"
                | "credentials"
                | "credentials.json"
                | "auth.json"
                | "identity.key"
                | ".git-credentials"
                | "secrets.toml"
                | "secrets.yaml"
                | "secrets.yml"
                | "secrets.json"
        )
}

fn pem() -> &'static regex::bytes::Regex {
    static RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::bytes::Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----").unwrap())
}

fn tokens() -> &'static regex::bytes::Regex {
    static RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::bytes::Regex::new(&super::secrets::token_pattern()).unwrap())
}

/// Why a copy wasn't made.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// It holds a credential that is never shared: a private key, one of
    /// Tessera's own keys, or (in a text file) a token in a known format.
    Never(String),
    /// It holds the user's saved passwords or keys (named), or something
    /// shaped like a token inside a binary file: shared only once the user
    /// says so.
    Holds(Vec<String>),
    Failed(String),
}

impl From<String> for Refusal {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

impl From<&str> for Refusal {
    fn from(e: &str) -> Self {
        Self::Failed(e.into())
    }
}

/// What a file may hold: (what it is, the value, never shared).
pub type Patterns = [(String, String, bool)];

/// Saved values as one search, and which value is what.
struct Saved {
    re: regex::bytes::Regex,
    what: std::collections::HashMap<Vec<u8>, (String, bool)>,
}

impl Saved {
    fn new(patterns: &Patterns, min: usize) -> Result<Option<Self>, Refusal> {
        let mut lits: Vec<&(String, String, bool)> =
            patterns.iter().filter(|(_, v, _)| v.len() >= min).collect();
        if lits.is_empty() {
            return Ok(None);
        }
        lits.sort_by_key(|(_, v, _)| std::cmp::Reverse(v.len()));
        let alts: Vec<String> = lits.iter().map(|(_, v, _)| regex::escape(v)).collect();
        let re = regex::bytes::Regex::new(&alts.join("|"))
            .map_err(|e| Refusal::Failed(format!("couldn't check the file: {e}")))?;
        let mut what = std::collections::HashMap::new();
        for (label, v, never) in lits {
            let e = what
                .entry(v.as_bytes().to_vec())
                .or_insert((label.clone(), *never));
            e.1 |= *never;
        }
        Ok(Some(Self { re, what }))
    }
}

/// What a piece of a file gives away: credentials never shared, and what
/// the user may still choose to share (added to `held`).
fn finds_secret(
    window: &[u8],
    saved: Option<&Saved>,
    binary: bool,
    held: &mut Vec<String>,
) -> Option<String> {
    if pem().is_match(window) {
        return Some("a private key".into());
    }
    let mut hold = |what: String| {
        if !held.contains(&what) {
            held.push(what);
        }
    };
    if let Some(saved) = saved {
        for m in saved.re.find_iter(window) {
            match saved.what.get(m.as_bytes()) {
                Some((label, true)) => return Some(label.clone()),
                Some((label, false)) => hold(label.clone()),
                None => {}
            }
        }
    }
    for m in tokens().find_iter(window) {
        let text = String::from_utf8_lossy(m.as_bytes());
        // Documentation placeholders don't count.
        let what = match super::secrets::find_known(&text).first() {
            Some(f) => format!("what looks like a {}", f.label),
            None if !text.contains("EXAMPLE") && !text.contains("xxxxxxxx") => {
                "what looks like an access token".into()
            }
            None => continue,
        };
        // Inside a program or picture, a match may be chance.
        if binary {
            hold(what);
        } else {
            return Some(what);
        }
    }
    None
}

/// Copy `src` to a new file `dst` (owner-only), refusing (and leaving
/// `dst` for the caller to remove) if the content holds a credential:
/// `patterns` are the saved ones. With `held_ok` (the user said yes) only
/// what is never shared refuses. Returns the size.
///
/// Compressed files (zip, APK, tar.gz…) are checked as they are, so what
/// is inside them isn't found.
pub fn copy_checked(
    src: &Path,
    dst: &Path,
    patterns: &Patterns,
    held_ok: bool,
) -> Result<u64, Refusal> {
    let input =
        super::file_object::Opened::open(src).map_err(|e| Refusal::Failed(e.to_string()))?;
    copy_checked_file(&input.file, &input.path, dst, patterns, held_ok)
}

pub fn copy_checked_file(
    input: &std::fs::File,
    src: &Path,
    dst: &Path,
    patterns: &Patterns,
    held_ok: bool,
) -> Result<u64, Refusal> {
    // Binary files only for longer values: a short one can turn up in
    // compiled code or pictures by chance.
    let saved_text = Saved::new(patterns, 8)?;
    let saved_binary = Saved::new(patterns, 12)?;
    let longest = patterns.iter().map(|(_, v, _)| v.len()).max().unwrap_or(0);
    let overlap = longest.max(4096) + 16;
    let failed = |e: String| Refusal::Failed(e);
    use std::io::{Seek, SeekFrom};
    let mut input = input.try_clone().map_err(|e| failed(e.to_string()))?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|e| failed(e.to_string()))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut out = opts
        .open(dst)
        .map_err(|e| failed(format!("couldn't store the copy: {e}")))?;
    let mut window: Vec<u8> = Vec::with_capacity((1 << 20) + overlap);
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    let mut binary = None;
    let mut held = Vec::new();
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| failed(format!("{}: {e}", src.display())))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_SHARE {
            return Err(failed(
                "the file grew past the size limit while it was copied".into(),
            ));
        }
        let binary = *binary.get_or_insert_with(|| buf[..n.min(8000)].contains(&0));
        window.extend_from_slice(&buf[..n]);
        let saved = if binary { &saved_binary } else { &saved_text };
        if let Some(what) = finds_secret(&window, saved.as_ref(), binary, &mut held) {
            return Err(Refusal::Never(format!(
                "Refused: {} contains {what}, and credentials are never shared. Share a copy without it (redacted), or tell the user where the file is.",
                src.display()
            )));
        }
        out.write_all(&buf[..n])
            .map_err(|e| failed(format!("couldn't store the copy: {e}")))?;
        if window.len() > overlap {
            window.drain(..window.len() - overlap);
        }
    }
    if !held.is_empty() && !held_ok {
        return Err(Refusal::Holds(held));
    }
    out.sync_all()
        .map_err(|e| failed(format!("couldn't store the copy: {e}")))?;
    Ok(total)
}

impl Tool for ShareFile {
    fn name(&self) -> &'static str {
        "share_file"
    }

    fn description(&self) -> String {
        "Give the user a file from this server as a download in the chat (phone or computer, up to 1 GB): reports, exports, images, builds. Credentials and Tessera's own files can't be shared.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "name": {"type": "string", "description": "Download name"}
            },
            "required": ["path"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        let Some(p) = args["path"].as_str() else {
            return Risk::None;
        };
        let path = ctx.resolve(p);
        match super::path_risk(&path, ctx) {
            Risk::Block { reason } => Risk::Block { reason },
            _ if secret_file(&path) || secret_file(&super::canonical(&path)) => Risk::Block {
                reason: format!(
                    "Refused: {} holds credentials, and credentials are never shared.",
                    path.display()
                ),
            },
            _ => Risk::None,
        }
    }

    fn summary(&self, args: &Value) -> String {
        args["path"].as_str().unwrap_or("").to_string()
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let Some(p) = args["path"].as_str().filter(|p| !p.trim().is_empty()) else {
                return ToolOutput::error("`path` is required");
            };
            // Checked again where it really points: a symlink may have been
            // swapped in since `assess`.
            let path = ctx.resolve(p);
            let opened = match super::file_object::Opened::open(&path) {
                Ok(file) => std::sync::Arc::new(file),
                Err(e) => return ToolOutput::error(e.to_string()),
            };
            // The risk follows the descriptor's path, not a fresh resolution
            // of a name another process can replace.
            if let Risk::Block { reason } = super::opened_path_risk(&opened.path, ctx) {
                return ToolOutput::error(reason);
            }
            if secret_file(&opened.path) {
                return ToolOutput::error(format!(
                    "Refused: {} holds credentials, and credentials are never shared.",
                    opened.path.display()
                ));
            }
            let name = args["name"].as_str().unwrap_or("").to_string();
            let mut held_ok = false;
            loop {
                let (host, session, file, name2) = (
                    ctx.host.clone(),
                    ctx.session_id.clone(),
                    opened.clone(),
                    name.clone(),
                );
                let shared = tokio::task::spawn_blocking(move || {
                    file.check_path()
                        .map_err(|e| Refusal::Failed(e.to_string()))?;
                    host.share_file(&session, &file.path, &file.file, &name2, held_ok)
                })
                .await
                .unwrap_or_else(|e| Err(Refusal::Failed(e.to_string())));
                match shared {
                    Ok(file) => {
                        return ToolOutput::text(format!(
                            "Shared {} ({}) in the chat; the user can download it from there.",
                            file.name,
                            crate::util::human_bytes(file.size)
                        ))
                        .with_details(json!({ "file": file }));
                    }
                    // The user's own saved values: theirs to decide, every
                    // time (YOLO included).
                    Err(Refusal::Holds(what)) if !held_ok => {
                        let allowed = ctx
                            .host
                            .approve_reason(super::Approve {
                                session_id: ctx.session_id.clone(),
                                call_id: ctx.call_id.clone(),
                                tool: "share_file".into(),
                                summary: opened.path.display().to_string(),
                                reason: format!(
                                    "This file contains {}. Sharing it puts a copy in this chat to download.",
                                    what.join(", ")
                                ),
                                key: format!("share:{}", opened.path.display()),
                                always: true,
                                cancel: ctx.cancel.clone(),
                                arguments: args.clone(),
                                once: false,
                            })
                            .await;
                        if let Err(why) = allowed {
                            return ToolOutput::error(why);
                        }
                        held_ok = true;
                    }
                    Err(Refusal::Never(e) | Refusal::Failed(e)) => return ToolOutput::error(e),
                    Err(Refusal::Holds(_)) => {
                        return ToolOutput::error("Not shared: the file holds saved secrets.");
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn sharing_copies_the_opened_object_not_a_replaced_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.txt");
        let other = dir.path().join("other.txt");
        let dst = dir.path().join("shared.txt");
        std::fs::write(&src, "ordinary notes").unwrap();
        std::fs::write(&other, "private fixture notes").unwrap();
        let input = super::super::file_object::Opened::open(&src).unwrap();
        std::fs::rename(&src, dir.path().join("saved.txt")).unwrap();
        symlink(&other, &src).unwrap();
        copy_checked_file(&input.file, &input.path, &dst, &Vec::new(), false).unwrap();
        assert_eq!(std::fs::read_to_string(dst).unwrap(), "ordinary notes");
    }

    #[test]
    fn credential_files_are_recognised_by_name() {
        for p in [
            "/srv/app/.env",
            "/srv/app/.env.production",
            "/home/u/.ssh/id_ed25519",
            "/tmp/id_rsa",
            "/etc/ssl/private/server.key",
            "/x/cert.pem",
            "/x/store.p12",
            "/x/vault.kdbx",
            "/home/u/.npmrc",
            "/home/u/.aws/credentials",
            "/x/auth.json",
        ] {
            assert!(secret_file(Path::new(p)), "{p}");
        }
        for p in [
            "/srv/app/.env.example",
            "/home/u/.ssh/id_ed25519.pub",
            "/x/report.pdf",
            "/x/keys.txt",
            "/x/monkey.png",
            "/x/id_card.jpg",
        ] {
            assert!(!secret_file(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn copies_are_scanned_for_secrets_across_chunk_edges() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.bin");
        let ok = dir.path().join("ok.bin");
        // Plain data, bigger than one chunk, is copied as it is.
        let data: Vec<u8> = (0..(3 << 20) + 17).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        assert_eq!(
            copy_checked(&src, &ok, &[], false).unwrap(),
            data.len() as u64
        );
        assert_eq!(std::fs::read(&ok).unwrap(), data);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&ok).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let check = |content: Vec<u8>, patterns: &Patterns, held_ok: bool| {
            std::fs::write(&src, content).unwrap();
            let out = dir.path().join(format!("o{}", rand::random::<u32>()));
            copy_checked(&src, &out, patterns, held_ok)
        };
        let saved = |label: &str, v: &str, never: bool| (label.to_string(), v.to_string(), never);
        // A saved password straddling the 1 MiB chunk boundary: the user
        // is asked (by name), and may say yes.
        let secret = "s3cr3t-value-0123456789";
        let mut content = vec![b'a'; (1 << 20) - 5];
        content.extend_from_slice(secret.as_bytes());
        content.extend_from_slice(&[b'b'; 100]);
        let pw = [saved("$NAS_PASSWORD", secret, false)];
        assert_eq!(
            check(content.clone(), &pw, false),
            Err(Refusal::Holds(vec!["$NAS_PASSWORD".into()]))
        );
        assert!(check(content.clone(), &pw, true).is_ok());
        // Tessera's own keys never go, yes or no.
        let own = [saved("the API key for openai", secret, true)];
        let e = check(content, &own, true).unwrap_err();
        assert!(
            matches!(&e, Refusal::Never(m) if m.contains("API key for openai")),
            "{e:?}"
        );
        let e = check(
            b"x\n-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n".to_vec(),
            &[],
            true,
        )
        .unwrap_err();
        assert!(
            matches!(&e, Refusal::Never(m) if m.contains("private key")),
            "{e:?}"
        );
        let e = check(
            b"token = ghp_0123456789abcdefghijABCDEFGHIJ012345 # mine".to_vec(),
            &[],
            true,
        )
        .unwrap_err();
        assert!(
            matches!(&e, Refusal::Never(m) if m.contains("looks like")),
            "{e:?}"
        );
        // Short "secrets" (a PIN) don't make everything unshareable.
        assert!(
            check(
                b"1234 is in the text".to_vec(),
                &[saved("$PIN", "1234", false)],
                false
            )
            .is_ok()
        );
    }

    /// Ordinary files that used to be refused: a program that happens to
    /// hold a saved value shorter than 12 characters (the APK), or text
    /// with a login's username, email or site in it.
    #[test]
    fn binaries_and_logins_do_not_trip_on_chance_matches() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("app.apk");
        let mut apk = b"PK\x03\x04\x00\x00".to_vec();
        apk.extend_from_slice(b"...testpw2pw...");
        apk.extend_from_slice(&[0u8; 64]);
        std::fs::write(&src, &apk).unwrap();
        let pw = [("$WIFI_PASSWORD".to_string(), "testpw2pw".to_string(), false)];
        assert!(copy_checked(&src, &dir.path().join("a"), &pw, false).is_ok());
        // The same value in a text file is found.
        std::fs::write(&src, b"wifi: testpw2pw\n").unwrap();
        assert_eq!(
            copy_checked(&src, &dir.path().join("b"), &pw, false),
            Err(Refusal::Holds(vec!["$WIFI_PASSWORD".into()]))
        );
        // Something shaped like a token inside a binary asks rather than refuses.
        let mut bin = vec![0u8; 16];
        bin.extend_from_slice(b"ghp_0123456789abcdefghijABCDEFGHIJ012345");
        std::fs::write(&src, &bin).unwrap();
        assert!(matches!(
            copy_checked(&src, &dir.path().join("c"), &[], false),
            Err(Refusal::Holds(_))
        ));
    }
}
