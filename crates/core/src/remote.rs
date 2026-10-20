//! Remote access through a Cloudflare tunnel, set up from one API token.
//!
//! [`Cloudflare::setup`] creates a tunnel that forwards a hostname to this
//! server, the DNS record for it, and an Access application that only lets a
//! dedicated service token through. Devices get that token's headers inside
//! their pairing QR code, and all traffic stays end-to-end encrypted anyway.
//! The API token is used for these calls only and never stored. The tunnel's
//! own run token and the service token are kept in `auth.json`.
//!
//! Tessera runs `cloudflared` itself ([`Supervisor`]), downloading the official
//! release when needed and verifying its SHA-256 against the digest GitHub
//! publishes for the release asset.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tessera_proto::CloudflareZone;

const API: &str = "https://api.cloudflare.com/client/v4";

/// What was created in the Cloudflare account (for status and removal).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunnelRecord {
    pub account_id: String,
    pub zone_id: String,
    pub hostname: String,
    pub tunnel_id: String,
    pub dns_record_id: String,
    pub access_app_id: String,
    pub service_token_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub run_token: String,
    /// The Access application that lets web browsers reach `/web` (made
    /// when "Use Tessera in a web browser" was first turned on; empty
    /// before).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub web_app_id: String,
    /// When the service token runs out (unix seconds): Cloudflare's last a
    /// year unless renewed. Unknown for tunnels set up before this was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_expires_at: Option<u64>,
}

/// How long Cloudflare's service tokens last, and what a renewal adds.
pub const KEY_LIFETIME_SECS: u64 = 365 * 86_400;

/// What to do when Cloudflare refuses Access: on an account without Zero
/// Trust it refuses every Access request, and its own words don't say why.
pub const ZERO_TRUST_HELP: &str = "Cloudflare didn't let Tessera use Access, the door that lets only your devices in. Usually that's because Zero Trust isn't turned on for this Cloudflare account yet. It's free: open https://one.dash.cloudflare.com, choose a team name and the Free plan, then try again. If it's on already, check that the token has Access: Apps and Policies · Edit and Access: Service Tokens · Edit.";

/// Which reminder about the devices' key is due `expires` (unix seconds)
/// at `now`: 1 from 30 days before it runs out, 2 from 7 days, 3 on the
/// last day, 4 once it has run out; `None` before.
pub fn key_reminder_stage(expires: u64, now: u64) -> Option<u8> {
    let left = expires.saturating_sub(now);
    match () {
        _ if expires <= now => Some(4),
        _ if left <= 86_400 => Some(3),
        _ if left <= 7 * 86_400 => Some(2),
        _ if left <= 30 * 86_400 => Some(1),
        _ => None,
    }
}

/// The notification for a reminder `stage`, `left` seconds before the key
/// runs out: plain words, and what to do.
pub fn key_reminder_text(stage: u8, left: u64) -> (&'static str, String) {
    const WHAT: &str =
        "The key your devices use to reach Tessera away from home (through Cloudflare)";
    const RENEW: &str = "Renew it in Settings → Connection: it takes a minute, and your devices keep working without pairing again.";
    if stage >= 4 {
        return (
            "Remote access stopped",
            format!(
                "{WHAT} has run out, so your devices can't connect away from home. Renew it in Settings → Connection (at home, or on the computer running Tessera), and they work again."
            ),
        );
    }
    let when = match left.div_ceil(86_400) {
        0 | 1 => "within a day".to_string(),
        d => format!("in {d} days"),
    };
    (
        "Remote access needs renewing",
        format!("{WHAT} runs out {when}. {RENEW}"),
    )
}

/// Why a Cloudflare API call failed.
enum CallError {
    /// Cloudflare couldn't be reached at all.
    Unreachable(anyhow::Error),
    /// Cloudflare answered no: its HTTP status and its own words.
    Refused { status: u16, said: String },
}

impl CallError {
    /// In words for the owner.
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            CallError::Unreachable(e) => e,
            CallError::Refused { status, said } => {
                if status == 403 || said.to_lowercase().contains("authentication") {
                    anyhow!(
                        "Cloudflare refused: {said}. Check the token has the permissions listed in the setup guide."
                    )
                } else {
                    anyhow!("Cloudflare: {said}")
                }
            }
        }
    }

    /// For calls to Access: the same, but a refusal says to turn Zero
    /// Trust on (or what the token needs), with Cloudflare's words after.
    fn access(self) -> anyhow::Error {
        match self {
            CallError::Refused { said, .. } => {
                anyhow!("{ZERO_TRUST_HELP} (Cloudflare said: {said})")
            }
            e => e.into_anyhow(),
        }
    }
}

/// Cloudflare's `2026-09-25T22:26:26Z` as unix seconds.
fn parse_time(v: &Value) -> Option<u64> {
    let t =
        time::OffsetDateTime::parse(v.as_str()?, &time::format_description::well_known::Rfc3339)
            .ok()?;
    u64::try_from(t.unix_timestamp()).ok()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Cloudflare {
    http: reqwest::Client,
    token: String,
    base: String,
}

/// A DNS name we are willing to put into API paths and queries.
fn valid_hostname(h: &str) -> bool {
    h.len() <= 253
        && h.split('.').count() >= 2
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Cloudflare object ids are hex; anything else never goes into a URL.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Cloudflare {
    pub fn new(http: reqwest::Client, token: &str) -> Self {
        Self {
            http,
            token: token.trim().to_string(),
            base: API.to_string(),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(method, path, body)
            .await
            .map_err(CallError::into_anyhow)
    }

    /// [`Cloudflare::call`] to Access (its applications and service
    /// tokens), where a refusal most often means Zero Trust is off.
    async fn access_call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(method, path, body)
            .await
            .map_err(CallError::access)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, CallError> {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(30));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb
            .send()
            .await
            .context("could not reach Cloudflare")
            .map_err(CallError::Unreachable)?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if v["success"].as_bool() != Some(true) {
            let errors: Vec<String> = v["errors"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e["message"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let said = if errors.is_empty() {
                format!("HTTP {status}")
            } else {
                errors.join("; ")
            };
            return Err(CallError::Refused {
                status: status.as_u16(),
                said,
            });
        }
        Ok(v["result"].clone())
    }

    /// Whether this token may use Access on `account` (`None`: Cloudflare
    /// couldn't be asked). On an account without Zero Trust, Access refuses.
    async fn access_ready(&self, account: &str) -> Option<bool> {
        if !valid_id(account) {
            return None;
        }
        match self
            .request(
                reqwest::Method::GET,
                &format!("/accounts/{account}/access/apps"),
                None,
            )
            .await
        {
            Ok(_) => Some(true),
            Err(CallError::Refused { .. }) => Some(false),
            Err(CallError::Unreachable(_)) => None,
        }
    }

    /// Checks the token works and lists the domains it can manage.
    pub async fn zones(&self) -> Result<Vec<CloudflareZone>> {
        let verify = self
            .call(reqwest::Method::GET, "/user/tokens/verify", None)
            .await;
        if let Err(e) = verify {
            bail!("that token doesn't work: {e}");
        }
        let v = self
            .call(
                reqwest::Method::GET,
                "/zones?per_page=50&status=active",
                None,
            )
            .await?;
        let mut zones: Vec<CloudflareZone> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|z| CloudflareZone {
                        id: z["id"].as_str().unwrap_or("").into(),
                        name: z["name"].as_str().unwrap_or("").into(),
                        account_id: z["account"]["id"].as_str().unwrap_or("").into(),
                        zero_trust: None,
                    })
                    .filter(|z| !z.id.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if zones.is_empty() {
            bail!(
                "the token works but can't see any domain. Give it Zone → DNS → Edit and Zone → Zone → Read for your domain."
            );
        }
        // Zero Trust is turned on per account: say so before the setup
        // starts rather than fail at its Access step.
        let mut asked: Vec<(String, Option<bool>)> = Vec::new();
        for z in &mut zones {
            let ready = match asked.iter().find(|(a, _)| *a == z.account_id) {
                Some((_, r)) => *r,
                None => {
                    let r = self.access_ready(&z.account_id).await;
                    asked.push((z.account_id.clone(), r));
                    r
                }
            };
            z.zero_trust = ready;
        }
        Ok(zones)
    }

    /// Create everything for `hostname` → `local_url`. Undoes what it made
    /// if a later step fails.
    pub async fn setup(
        &self,
        zone: &CloudflareZone,
        hostname: &str,
        local_url: &str,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<TunnelRecord> {
        let hostname = hostname.trim().trim_end_matches('.').to_ascii_lowercase();
        let on_zone = hostname.ends_with(&format!(".{}", zone.name)) || hostname == zone.name;
        if !on_zone || !valid_hostname(&hostname) {
            bail!(
                "the address must be on {}, e.g. tessera.{}",
                zone.name,
                zone.name
            );
        }
        if !valid_id(&zone.id) || !valid_id(&zone.account_id) {
            bail!("unexpected zone id from Cloudflare");
        }
        let a = &zone.account_id;
        progress("Checking the address is free");
        let existing = self
            .call(
                reqwest::Method::GET,
                &format!("/zones/{}/dns_records?name={hostname}", zone.id),
                None,
            )
            .await?;
        if existing.as_array().is_some_and(|r| !r.is_empty()) {
            bail!("{hostname} is already in use in your DNS; pick another name");
        }
        // Access first: on an account without Zero Trust this is where it
        // stops, before anything is created.
        let apps = self
            .access_call(
                reqwest::Method::GET,
                &format!("/accounts/{a}/access/apps"),
                None,
            )
            .await?;
        if apps.as_array().is_some_and(|l| {
            l.iter()
                .any(|x| x["domain"].as_str() == Some(hostname.as_str()))
        }) {
            bail!("an Access application already protects {hostname}; pick another name");
        }

        let mut created = Partial::default();
        let result: Result<TunnelRecord> = async {
            progress("Creating the tunnel");
            let short: String = hostname.split('.').next().unwrap_or("tessera").chars().take(20).collect();
            let name = format!("tessera-{short}-{}", &crate::util::new_id()[16..]);
            let t = self.call(reqwest::Method::POST, &format!("/accounts/{a}/cfd_tunnel"), Some(json!({"name": name, "config_src": "cloudflare"}))).await?;
            let tunnel_id = t["id"].as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("no tunnel id"))?.to_string();
            created.tunnel = Some(tunnel_id.clone());
            let ingress = json!({"config": {"ingress": [
                {"hostname": hostname, "service": local_url, "originRequest": {"connectTimeout": 10}},
                {"service": "http_status:404"}
            ]}});
            self.call(reqwest::Method::PUT, &format!("/accounts/{a}/cfd_tunnel/{tunnel_id}/configurations"), Some(ingress)).await?;

            progress("Adding the DNS record");
            let dns = self
                .call(
                    reqwest::Method::POST,
                    &format!("/zones/{}/dns_records", zone.id),
                    Some(json!({"type": "CNAME", "name": hostname, "content": format!("{tunnel_id}.cfargotunnel.com"), "proxied": true, "comment": "Tessera tunnel"})),
                )
                .await?;
            let dns_record_id = dns["id"].as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("no DNS record id"))?.to_string();
            created.dns = Some((zone.id.clone(), dns_record_id.clone()));

            progress("Locking it with Cloudflare Access");
            let st = self.access_call(reqwest::Method::POST, &format!("/accounts/{a}/access/service_tokens"), Some(json!({"name": format!("Tessera apps ({hostname})"), "duration": "8760h"}))).await?;
            let key_expires_at = parse_time(&st["expires_at"]).unwrap_or_else(|| now_secs() + KEY_LIFETIME_SECS);
            let service_token_id = st["id"].as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("no service token id"))?.to_string();
            created.service_token = Some(service_token_id.clone());
            let client_id = st["client_id"].as_str().ok_or_else(|| anyhow!("no service token id"))?.to_string();
            let client_secret = st["client_secret"].as_str().ok_or_else(|| anyhow!("no service token secret"))?.to_string();
            let app = self
                .access_call(
                    reqwest::Method::POST,
                    &format!("/accounts/{a}/access/apps"),
                    Some(json!({
                        "name": format!("Tessera ({hostname})"), "domain": hostname, "type": "self_hosted", "session_duration": "24h",
                        "app_launcher_visible": false, "auto_redirect_to_identity": false, "service_auth_401_redirect": true,
                        "policies": [{"name": "Tessera apps", "decision": "non_identity", "include": [{"service_token": {"token_id": service_token_id}}], "precedence": 1}]
                    })),
                )
                .await?;
            let access_app_id = app["id"].as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("no Access application id"))?.to_string();
            created.app = Some(access_app_id.clone());

            let run = self.call(reqwest::Method::GET, &format!("/accounts/{a}/cfd_tunnel/{tunnel_id}/token"), None).await?;
            let run_token = run.as_str().ok_or_else(|| anyhow!("no tunnel token"))?.to_string();
            Ok(TunnelRecord {
                account_id: a.clone(),
                zone_id: zone.id.clone(),
                hostname: hostname.clone(),
                tunnel_id,
                dns_record_id,
                access_app_id,
                service_token_id,
                client_id,
                client_secret,
                run_token,
                web_app_id: String::new(),
                key_expires_at: Some(key_expires_at),
            })
        }
        .await;
        if let Err(ref error) = result {
            progress("Something failed; undoing what was created");
            if let Err(cleanup) = self.undo(a, &created).await {
                return Err(anyhow!("{error}; cleanup also failed: {cleanup}"));
            }
        }
        result
    }

    /// "Use Tessera in a web browser": let browsers reach `/web` on the
    /// tunnel's address. The Access application set up with the tunnel lets
    /// only the apps' service token through, and a browser can't send it;
    /// this adds one for `<hostname>/web` (Access goes by the most specific
    /// path) that lets everyone reach the web app and its sign-in, where
    /// only browsers paired with a code from the user's devices get further.
    /// The rest of the address stays as locked as before. Returns its id
    /// (an existing one for the same path is taken as it is).
    pub async fn web_door(&self, r: &TunnelRecord) -> Result<String> {
        if !valid_id(&r.account_id) || !valid_hostname(&r.hostname) {
            bail!("unexpected tunnel record");
        }
        let a = &r.account_id;
        let domain = format!("{}/web", r.hostname);
        let apps = self
            .access_call(
                reqwest::Method::GET,
                &format!("/accounts/{a}/access/apps"),
                None,
            )
            .await?;
        if let Some(id) = apps.as_array().and_then(|l| {
            l.iter()
                .find(|x| x["domain"].as_str() == Some(domain.as_str()))
                .and_then(|x| x["id"].as_str())
                .filter(|i| valid_id(i))
        }) {
            return Ok(id.to_string());
        }
        let app = self
            .access_call(
                reqwest::Method::POST,
                &format!("/accounts/{a}/access/apps"),
                Some(json!({
                    "name": format!("Tessera in a web browser ({})", r.hostname), "domain": domain, "type": "self_hosted",
                    "session_duration": "24h", "app_launcher_visible": false, "auto_redirect_to_identity": false,
                    "policies": [{"name": "Web browsers (Tessera checks them itself)", "decision": "bypass", "include": [{"everyone": {}}], "precedence": 1}]
                })),
            )
            .await?;
        app["id"]
            .as_str()
            .filter(|i| valid_id(i))
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no Access application id"))
    }

    async fn undo(&self, a: &str, p: &Partial) -> Result<()> {
        if !valid_id(a) {
            bail!("unexpected Cloudflare account id; nothing was removed");
        }
        let mut access_deletions = Vec::new();
        for (label, id, kind) in [
            ("web Access application", &p.web_app, "apps"),
            ("Access application", &p.app, "apps"),
            ("devices' service token", &p.service_token, "service_tokens"),
        ] {
            if let Some(id) = id {
                if !valid_id(id) {
                    bail!("unexpected Cloudflare resource id; nothing was removed");
                }
                access_deletions.push((label, format!("/accounts/{a}/access/{kind}/{id}")));
            }
        }
        let mut deletions = Vec::new();
        if let Some((zone, rec)) = &p.dns {
            if !valid_id(zone) || !valid_id(rec) {
                bail!("unexpected DNS record id; nothing was removed");
            }
            deletions.push(("DNS record", format!("/zones/{zone}/dns_records/{rec}")));
        }
        if let Some(t) = &p.tunnel {
            if !valid_id(t) {
                bail!("unexpected tunnel id; nothing was removed");
            }
            deletions.push((
                "tunnel connections",
                format!("/accounts/{a}/cfd_tunnel/{t}/connections"),
            ));
            deletions.push(("tunnel", format!("/accounts/{a}/cfd_tunnel/{t}")));
        }
        let ingress_steps = deletions.len();
        deletions.extend(access_deletions);
        // Do not weaken Access until every route to the origin is gone.
        // Stop at the first failure so all later resources remain for retry.
        for (index, (label, path)) in deletions.iter().enumerate() {
            match self.request(reqwest::Method::DELETE, path, None).await {
                Ok(_)
                | Err(CallError::Refused {
                    status: 404 | 410, ..
                }) => {}
                Err(e) => {
                    let remaining = deletions[index..]
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let protection = if index < ingress_steps {
                        " Access protection and the devices' key have not been removed."
                    } else {
                        ""
                    };
                    bail!(
                        "Cloudflare removal is incomplete. Could not remove {label} ({error}). Still to remove or check: {remaining}.{protection} Check the API token's permissions and try again.",
                        error = e.into_anyhow()
                    );
                }
            }
        }
        Ok(())
    }

    /// Renew the service token the devices carry (Cloudflare's "Refresh":
    /// another year, the same key, so devices need nothing new). Needs
    /// Access: Service Tokens · Edit. Returns when it runs out now.
    pub async fn renew(&self, r: &TunnelRecord) -> Result<u64> {
        if !valid_id(&r.account_id) || !valid_id(&r.service_token_id) {
            bail!("unexpected tunnel record");
        }
        self.call(reqwest::Method::GET, "/user/tokens/verify", None)
            .await
            .map_err(|e| anyhow!("that token doesn't work: {e}"))?;
        let path = format!(
            "/accounts/{}/access/service_tokens/{}/refresh",
            r.account_id, r.service_token_id
        );
        let st = match self.request(reqwest::Method::POST, &path, None).await {
            Ok(v) => v,
            Err(CallError::Refused { status: 404, .. }) => bail!(
                "Cloudflare no longer has the key Tessera made for your devices (it was deleted). Remove remote access and set it up again."
            ),
            Err(CallError::Refused { status, said })
                if status == 403 || said.to_lowercase().contains("authentication") =>
            {
                bail!(
                    "Cloudflare refused: {said}. The token needs Account · Access: Service Tokens · Edit."
                )
            }
            Err(e) => return Err(e.access()),
        };
        Ok(parse_time(&st["expires_at"]).unwrap_or_else(|| now_secs() + KEY_LIFETIME_SECS))
    }

    /// Delete everything [`setup`] created. On failure the caller must keep
    /// the record for retry (already-deleted resources then count as done).
    pub async fn remove(&self, r: &TunnelRecord) -> Result<()> {
        self.call(reqwest::Method::GET, "/user/tokens/verify", None)
            .await
            .map_err(|e| anyhow!("that token doesn't work: {e}"))?;
        let p = Partial {
            tunnel: Some(r.tunnel_id.clone()),
            dns: Some((r.zone_id.clone(), r.dns_record_id.clone())),
            service_token: Some(r.service_token_id.clone()),
            app: Some(r.access_app_id.clone()),
            web_app: Some(r.web_app_id.clone()).filter(|i| !i.is_empty()),
        };
        self.undo(&r.account_id, &p).await.map_err(|e| {
            anyhow!("{e} The saved resource IDs are kept for another removal attempt.")
        })
    }
}

#[derive(Default)]
struct Partial {
    tunnel: Option<String>,
    dns: Option<(String, String)>,
    service_token: Option<String>,
    app: Option<String>,
    web_app: Option<String>,
}

fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("cloudflared-linux-amd64"),
        ("linux", "aarch64") => Some("cloudflared-linux-arm64"),
        ("linux", "arm") => Some("cloudflared-linux-arm"),
        ("windows", "x86_64") => Some("cloudflared-windows-amd64.exe"),
        _ => None,
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

/// An installed cloudflared: Tessera's own copy first, then the system's.
pub fn find_cloudflared(data_dir: &Path) -> Option<PathBuf> {
    let own = data_dir.join("bin").join(binary_name());
    if own.is_file() {
        return Some(own);
    }
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    dirs.extend(dirs::home_dir().map(|h| h.join(".local/bin")));
    dirs.into_iter()
        .map(|d| d.join(binary_name()))
        .find(|p| p.is_file())
}

// Downloads are public and independent of the credential-bearing provider client.
// GitHub redirects release assets; never downgrade HTTPS or accept URL credentials.
fn cloudflared_client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if !attempt.url().username().is_empty() || attempt.url().password().is_some() {
                attempt.error("cloudflared download redirects must not contain credentials")
            } else {
                reqwest::redirect::Policy::limited(5).redirect(attempt)
            }
        }))
}

/// Find cloudflared, or download the latest official release and verify it.
pub async fn ensure_cloudflared(
    data_dir: &Path,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<PathBuf> {
    if let Some(p) = find_cloudflared(data_dir) {
        return Ok(p);
    }
    let asset = asset_name().ok_or_else(|| {
        anyhow!("no cloudflared build for this platform; install it yourself and try again")
    })?;
    download_cloudflared(
        &cloudflared_client()
            .build()
            .context("building download client")?,
        "https://api.github.com/repos/cloudflare/cloudflared/releases/latest",
        asset,
        data_dir,
        progress,
    )
    .await
}

async fn download_cloudflared(
    http: &reqwest::Client,
    release_url: &str,
    asset: &str,
    data_dir: &Path,
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<PathBuf> {
    progress("Downloading cloudflared from Cloudflare's GitHub releases");
    let release: Value = http
        .get(release_url)
        .header("User-Agent", "tessera")
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let a = release["assets"]
        .as_array()
        .and_then(|l| l.iter().find(|x| x["name"].as_str() == Some(asset)))
        .ok_or_else(|| anyhow!("the release has no {asset}"))?;
    let url = a["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow!("no download URL"))?;
    let url = reqwest::Url::parse(url)?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("cloudflared download URLs must not contain credentials");
    }
    let want = a["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_string)
        .or_else(|| {
            // Older releases list checksums in the notes: `name: <sha256>`.
            let body = release["body"].as_str()?;
            body.lines().find_map(|l| {
                l.trim()
                    .strip_prefix(&format!("{asset}:"))
                    .map(|h| h.trim().to_string())
            })
        });
    let Some(want) = want.filter(|h| h.len() == 64) else {
        bail!(
            "couldn't find the published checksum for {asset}; not installing an unverified binary"
        )
    };
    let bytes = http
        .get(url)
        .header("User-Agent", "tessera")
        .timeout(Duration::from_secs(300))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let got = hex(&Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(&want) {
        bail!("the downloaded cloudflared doesn't match its published checksum; not installing it");
    }
    let dir = data_dir.join("bin");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(binary_name());
    let tmp = dir.join(format!("{}.tmp", binary_name()));
    std::fs::write(&tmp, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What the supervised cloudflared is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TunnelState {
    /// The process is running.
    pub running: bool,
    /// It has a live connection to Cloudflare.
    pub connected: bool,
    /// Why it isn't running or connected, in words for the owner.
    pub error: Option<String>,
}

/// Where cloudflared connects (TCP and UDP port 7844).
const EDGE: &str = "region1.v2.argotunnel.com:7844";
/// How long a fresh cloudflared gets to connect before it counts as stuck.
const CONNECT_GRACE: Duration = Duration::from_secs(20);

/// Keeps `cloudflared tunnel run` going, restarting it with backoff, and
/// watches its `/ready` endpoint to know whether it is really connected.
pub struct Supervisor {
    stop: tokio_util::sync::CancellationToken,
    finished: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
    state: Arc<Mutex<TunnelState>>,
}

impl Supervisor {
    pub fn start(binary: PathBuf, token_file: PathBuf) -> Self {
        let stop = tokio_util::sync::CancellationToken::new();
        let state = Arc::new(Mutex::new(TunnelState::default()));
        let (finished_tx, finished) = tokio::sync::watch::channel(None);
        let (stop2, state2) = (stop.clone(), state.clone());
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap_or_default();
            let mut backoff = Duration::from_secs(2);
            while !stop2.is_cancelled() {
                let metrics = free_port().unwrap_or(20241);
                let mut cmd = tokio::process::Command::new(&binary);
                cmd.args(["tunnel", "--no-autoupdate", "--metrics"])
                    .arg(format!("127.0.0.1:{metrics}"))
                    .args(["run", "--token-file"])
                    .arg(&token_file)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true);
                // Windows: no window, and it ends with the server even when
                // the server doesn't stop cleanly (a crash, the watchdog).
                let child = crate::platform::spawn(&mut cmd, crate::platform::Start::Quiet);
                let mut child = match child {
                    Ok(c) => c,
                    Err(e) => {
                        *state2.lock() = TunnelState {
                            error: Some(format!("cloudflared didn't start: {e}")),
                            ..Default::default()
                        };
                        tokio::select! { _ = tokio::time::sleep(backoff) => {}, _ = stop2.cancelled() => break }
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        continue;
                    }
                };
                *state2.lock() = TunnelState {
                    running: true,
                    ..Default::default()
                };
                let started = std::time::Instant::now();
                let mut told = false;
                let mut was_connected = false;
                let exited = loop {
                    tokio::select! {
                        status = child.wait() => break Some(status),
                        _ = stop2.cancelled() => {
                            if let Err(e) = child.kill().await {
                                let error = format!("cloudflared could not be stopped ({e})");
                                state2.lock().error = Some(error.clone());
                                let _ = finished_tx.send(Some(Err(error)));
                                return;
                            }
                            break None;
                        }
                        // Every 5 s until connected, then every 30 s: a
                        // drop is noticed a little later, and an idle
                        // server wakes six times less often for it.
                        _ = tokio::time::sleep(Duration::from_secs(if was_connected { 30 } else { 5 })) => {
                            let connected = ready_connections(&http, metrics).await.is_some_and(|n| n > 0);
                            if connected {
                                if !was_connected {
                                    tracing::info!("Cloudflare tunnel connected");
                                }
                                was_connected = true;
                                told = false;
                                *state2.lock() = TunnelState { running: true, connected: true, error: None };
                            } else if started.elapsed() >= CONNECT_GRACE {
                                let why = diagnose(EDGE).await;
                                if !told {
                                    tracing::warn!("Cloudflare tunnel isn't connected: {why}");
                                    told = true;
                                }
                                was_connected = false;
                                *state2.lock() = TunnelState { running: true, connected: false, error: Some(why) };
                            }
                        }
                    }
                };
                let Some(status) = exited else { break };
                let msg = format!(
                    "cloudflared exited ({})",
                    status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| e.to_string())
                );
                tracing::warn!("{msg}; restarting");
                *state2.lock() = TunnelState {
                    error: Some(msg),
                    ..Default::default()
                };
                if started.elapsed() > Duration::from_secs(60) {
                    backoff = Duration::from_secs(2);
                }
                tokio::select! { _ = tokio::time::sleep(backoff) => {}, _ = stop2.cancelled() => break }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
            *state2.lock() = TunnelState::default();
            let _ = finished_tx.send(Some(Ok(())));
        });
        Self {
            stop,
            finished,
            state,
        }
    }

    pub fn state(&self) -> TunnelState {
        self.state.lock().clone()
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }

    /// Confirm the managed connector has exited before changing Access.
    pub async fn stop_and_wait(&mut self) -> Result<()> {
        self.stop();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(result) = self.finished.borrow().clone() {
                    return result.map_err(anyhow::Error::msg);
                }
                self.finished.changed().await.map_err(|_| {
                    anyhow!("cloudflared stopped without confirming that it exited")
                })?;
            }
        })
        .await
        .map_err(|_| {
            anyhow!(
                "cloudflared has not confirmed it stopped. Cloudflare resources were left in place"
            )
        })?
    }
}

/// A free local port for cloudflared's status endpoint.
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .ok()
}

/// Live connections cloudflared reports on its `/ready` endpoint (it
/// answers 503 with a count of 0 while it isn't connected).
async fn ready_connections(http: &reqwest::Client, port: u16) -> Option<u64> {
    let r = http
        .get(format!("http://127.0.0.1:{port}/ready"))
        .send()
        .await
        .ok()?;
    let v: Value = r.json().await.ok()?;
    v["readyConnections"].as_u64()
}

/// Why a running cloudflared has no connection, for the owner: most often a
/// firewall that doesn't let this machine out on Cloudflare's tunnel port.
async fn diagnose(edge: &str) -> String {
    let open = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(edge))
        .await
        .is_ok_and(|r| r.is_ok());
    if open {
        "cloudflared is running but isn't connected to Cloudflare; check that the tunnel still exists in your Cloudflare account".into()
    } else {
        "this machine can't reach Cloudflare's tunnel servers: outbound port 7844 (TCP and UDP) looks blocked by a firewall. Allow it out to region1.v2.argotunnel.com and region2.v2.argotunnel.com, and the tunnel connects by itself".into()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[cfg(test)]
mod cloudflared_download_tests;
#[cfg(test)]
mod tests {
    use super::*;

    // Reserve a closed port throughout each negative probe. A dropped listener
    // from free_port() can be reused by a concurrent test before the connect.
    fn reserved_closed_port() -> tokio::net::TcpSocket {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        socket
    }

    #[tokio::test]
    async fn the_connection_count_is_read_from_cloudflareds_ready_endpoint() {
        use axum::{Router, http::StatusCode, routing::get};
        let serve = |code: StatusCode, n: u64| async move {
            let app = Router::new().route(
                "/ready",
                get(move || async move {
                    (
                        code,
                        axum::Json(json!({"status": code.as_u16(), "readyConnections": n})),
                    )
                }),
            );
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
            port
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let up = serve(StatusCode::OK, 4).await;
        assert_eq!(ready_connections(&http, up).await, Some(4));
        let down = serve(StatusCode::SERVICE_UNAVAILABLE, 0).await;
        assert_eq!(ready_connections(&http, down).await, Some(0));
        let nothing = reserved_closed_port();
        assert_eq!(
            ready_connections(&http, nothing.local_addr().unwrap().port()).await,
            None
        );
        drop(nothing);
    }

    #[tokio::test]
    async fn a_blocked_tunnel_port_is_named() {
        let reservation = reserved_closed_port();
        let closed = reservation.local_addr().unwrap().to_string();
        assert!(diagnose(&closed).await.contains("outbound port 7844"));
        drop(reservation);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = l.local_addr().unwrap().to_string();
        assert!(
            diagnose(&open)
                .await
                .contains("isn't connected to Cloudflare")
        );
    }

    #[test]
    fn only_plain_dns_names_are_accepted() {
        for ok in ["tessera.example.com", "a-b.example.co.uk", "x1.example.com"] {
            assert!(valid_hostname(ok), "{ok}");
        }
        for bad in [
            "example",
            "a..example.com",
            "-a.example.com",
            "a b.example.com",
            "a.example.com&x=1",
            "a.example.com#",
            "a/b.example.com",
            "ünï.example.com",
        ] {
            assert!(!valid_hostname(bad), "{bad}");
        }
        assert!(valid_id("023e105f4ecef8ad9ca31a8372d0c353"));
        assert!(!valid_id("../../zones") && !valid_id("a?b") && !valid_id(""));
    }

    #[test]
    fn reminders_start_a_month_before_the_key_runs_out() {
        let day = 86_400;
        let exp = 1_000 * day;
        assert_eq!(key_reminder_stage(exp, exp - 31 * day), None);
        assert_eq!(key_reminder_stage(exp, exp - 30 * day), Some(1));
        assert_eq!(key_reminder_stage(exp, exp - 8 * day), Some(1));
        assert_eq!(key_reminder_stage(exp, exp - 7 * day), Some(2));
        assert_eq!(key_reminder_stage(exp, exp - day + 5), Some(3));
        assert_eq!(key_reminder_stage(exp, exp), Some(4));
        assert_eq!(key_reminder_stage(exp, exp + 400 * day), Some(4));
        let (title, body) = key_reminder_text(1, 30 * day);
        assert_eq!(title, "Remote access needs renewing");
        assert!(
            body.contains("in 30 days") && body.contains("Settings → Connection"),
            "{body}"
        );
        assert!(key_reminder_text(3, 3600).1.contains("within a day"));
        let (title, body) = key_reminder_text(4, 0);
        assert_eq!(title, "Remote access stopped");
        assert!(body.contains("has run out"), "{body}");
    }

    #[test]
    fn cloudflare_times_are_read() {
        assert_eq!(
            parse_time(&json!("2026-09-25T22:26:26Z")),
            Some(1_790_375_186)
        );
        assert_eq!(parse_time(&json!("soon")), None);
        assert_eq!(parse_time(&Value::Null), None);
    }

    #[test]
    fn this_platform_has_a_cloudflared_build_when_expected() {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(asset_name(), Some("cloudflared-linux-amd64"));
        }
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_connector_is_reaped_before_stop_returns() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("fixture-connector");
        std::fs::write(&binary, b"#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut connector = Supervisor::start(binary, dir.path().join("unused-token"));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !connector.state().running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        connector.stop_and_wait().await.unwrap();
        assert!(!connector.state().running);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unconfirmed_connector_stop_fails_closed() {
        let (_tx, finished) = tokio::sync::watch::channel(None);
        let mut connector = Supervisor {
            stop: tokio_util::sync::CancellationToken::new(),
            finished,
            state: Arc::default(),
        };
        let error = connector.stop_and_wait().await.unwrap_err().to_string();
        assert!(error.contains("resources were left in place"));
    }

    /// A fake Cloudflare API that fails at a chosen call and records calls.
    async fn fake_cloudflare(fail_on: &'static str) -> (String, Calls) {
        use axum::extract::{Path as P, State};
        use axum::http::Method;
        async fn any(
            State((calls, fail_on)): State<(Calls, &'static str)>,
            method: Method,
            P(path): P<String>,
            body: String,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let call = format!("{method} /{path}");
            calls.lock().push(call.clone());
            if !body.is_empty() {
                calls.lock().push(format!("BODY {body}"));
            }
            let ok = |result: Value| {
                axum::Json(json!({"success": true, "result": result})).into_response()
            };
            let failed = json!({"success": false, "errors": [{"message": "simulated failure"}]});
            for refused in fail_on.split(';') {
                if call == refused {
                    return axum::Json(failed).into_response();
                }
                for status in [403, 404, 500] {
                    if refused.strip_prefix(&format!("{status} ")) == Some(call.as_str()) {
                        return (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            axum::Json(failed),
                        )
                            .into_response();
                    }
                }
            }
            match (method, path.as_str()) {
                (Method::GET, "zones") => {
                    ok(json!([{"id": "zone1", "name": "example.com", "account": {"id": "acct1"}}]))
                }
                (Method::POST, p) if p.ends_with("/refresh") => {
                    ok(json!({"id": "st1", "expires_at": "2027-10-07T00:00:00Z"}))
                }
                (Method::GET, p) if p.ends_with("dns_records") => ok(json!([])),
                (Method::GET, p) if p.ends_with("access/apps") => ok(json!([])),
                (Method::GET, p) if p.ends_with("/token") => ok(json!("run-token-value")),
                (Method::POST, p) if p.ends_with("cfd_tunnel") => ok(json!({"id": "tun1"})),
                (Method::POST, p) if p.ends_with("dns_records") => ok(json!({"id": "dns1"})),
                (Method::POST, p) if p.ends_with("service_tokens") => ok(
                    json!({"id": "st1", "client_id": "cid.access", "client_secret": "csecret", "expires_at": "2026-09-25T22:26:26Z"}),
                ),
                (Method::POST, p) if p.ends_with("access/apps") => {
                    ok(json!({"id": if body.contains("/web\"") { "app2" } else { "app1" }}))
                }
                _ => ok(json!({})),
            }
        }
        let calls: Calls = Arc::default();
        let app = axum::Router::new()
            .route("/{*path}", axum::routing::any(any))
            .with_state((calls.clone(), fail_on));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, calls)
    }

    fn zone() -> CloudflareZone {
        CloudflareZone {
            id: "zone1".into(),
            name: "example.com".into(),
            account_id: "acct1".into(),
            zero_trust: None,
        }
    }

    fn client(base: String) -> Cloudflare {
        Cloudflare {
            http: reqwest::Client::new(),
            token: "t".into(),
            base,
        }
    }

    #[tokio::test]
    async fn removal_stops_at_each_failure_and_keeps_access_until_routes_are_gone() {
        let (base, _) = fake_cloudflare("never").await;
        let mut rec = client(base)
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:9999",
                &|_| {},
            )
            .await
            .unwrap();
        rec.web_app_id = "app2".into();
        let before = rec.clone();
        let ordered = [
            "DELETE /zones/zone1/dns_records/dns1",
            "DELETE /accounts/acct1/cfd_tunnel/tun1/connections",
            "DELETE /accounts/acct1/cfd_tunnel/tun1",
            "DELETE /accounts/acct1/access/apps/app2",
            "DELETE /accounts/acct1/access/apps/app1",
            "DELETE /accounts/acct1/access/service_tokens/st1",
        ];
        for (index, failed) in ordered.iter().enumerate() {
            let (base, calls) = fake_cloudflare(failed).await;
            let error = client(base).remove(&rec).await.unwrap_err().to_string();
            assert!(
                error.contains("removal is incomplete")
                    && error.contains("Still to remove or check"),
                "{error}"
            );
            assert!(error.contains("saved resource IDs are kept"), "{error}");
            if index < 3 {
                assert!(
                    error.contains("Access protection and the devices' key have not been removed"),
                    "{error}"
                );
                assert!(!calls.lock().iter().any(|c| c.contains("/access/")));
            }
            let deletes: Vec<_> = calls
                .lock()
                .iter()
                .filter(|c| c.starts_with("DELETE"))
                .cloned()
                .collect();
            assert_eq!(deletes, ordered[..=index]);
            assert_eq!(rec, before, "failure retains the complete retry record");
        }
        // Missing resources count as done on retry, in the same safe order.
        let (base, calls) = fake_cloudflare("404 DELETE /accounts/acct1/access/apps/app2;404 DELETE /accounts/acct1/access/service_tokens/st1;404 DELETE /accounts/acct1/cfd_tunnel/tun1/connections;404 DELETE /accounts/acct1/cfd_tunnel/tun1").await;
        client(base).remove(&rec).await.unwrap();
        assert_eq!(
            calls
                .lock()
                .iter()
                .filter(|c| c.starts_with("DELETE"))
                .cloned()
                .collect::<Vec<_>>(),
            ordered
        );
    }

    #[tokio::test]
    async fn removal_does_not_ignore_invalid_resource_ids() {
        let (base, calls) = fake_cloudflare("never").await;
        let cf = client(base);
        let mut rec = cf
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:9999",
                &|_| {},
            )
            .await
            .unwrap();
        calls.lock().clear();
        rec.web_app_id = "../other".into();
        assert!(cf.remove(&rec).await.is_err());
        assert!(!calls.lock().iter().any(|c| c.starts_with("DELETE")));
    }

    #[tokio::test]
    async fn a_failed_setup_removes_everything_it_created() {
        let (base, calls) = fake_cloudflare("POST /accounts/acct1/access/apps").await;
        let err = client(base)
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("simulated failure"), "{err}");
        let calls = calls.lock().clone();
        for undo in [
            "DELETE /accounts/acct1/access/service_tokens/st1",
            "DELETE /zones/zone1/dns_records/dns1",
            "DELETE /accounts/acct1/cfd_tunnel/tun1",
        ] {
            assert!(
                calls.iter().any(|c| c == undo),
                "missing {undo}: {calls:#?}"
            );
        }
    }

    #[tokio::test]
    async fn setup_records_what_it_made_and_refuses_foreign_names() {
        let (base, calls) = fake_cloudflare("never").await;
        let cf = client(base);
        let rec = cf
            .setup(
                &zone(),
                "Tessera.Example.com.",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap();
        assert_eq!(rec.hostname, "tessera.example.com");
        assert_eq!(
            (
                rec.tunnel_id.as_str(),
                rec.dns_record_id.as_str(),
                rec.access_app_id.as_str()
            ),
            ("tun1", "dns1", "app1")
        );
        assert_eq!(rec.run_token, "run-token-value");
        assert_eq!(
            rec.key_expires_at,
            Some(1_790_375_186),
            "when the devices' key runs out, as Cloudflare said"
        );
        assert!(!calls.lock().iter().any(|c| c.starts_with("DELETE")));
        for bad in [
            "tessera.other.com",
            "x.example.com&name=y",
            "evil.com/.example.com",
        ] {
            assert!(
                cf.setup(&zone(), bad, "http://127.0.0.1:7878", &|_| {})
                    .await
                    .is_err(),
                "{bad}"
            );
        }
    }

    /// A new account without Zero Trust: Cloudflare refuses Access. The
    /// token check says so per domain, and the setup stops before it
    /// creates anything, saying what to do in plain words.
    #[tokio::test]
    async fn an_account_without_zero_trust_is_named_before_anything_is_made() {
        let (base, calls) = fake_cloudflare("GET /accounts/acct1/access/apps").await;
        let cf = client(base);
        let zones = cf.zones().await.unwrap();
        assert_eq!(zones[0].zero_trust, Some(false));
        calls.lock().clear();
        let err = cf
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Zero Trust isn't turned on"), "{err}");
        assert!(err.contains("https://one.dash.cloudflare.com"), "{err}");
        assert!(
            err.contains("simulated failure"),
            "Cloudflare's own words follow: {err}"
        );
        let made = calls.lock().clone();
        assert!(
            !made
                .iter()
                .any(|c| c.starts_with("POST") || c.starts_with("DELETE")),
            "nothing made or undone: {made:#?}"
        );
        // With Zero Trust on, the check says so.
        let (base, _) = fake_cloudflare("never").await;
        assert_eq!(
            client(base).zones().await.unwrap()[0].zero_trust,
            Some(true)
        );
        // Refused later (making the devices' key): the same words.
        let (base, _) = fake_cloudflare("POST /accounts/acct1/access/service_tokens").await;
        let err = client(base)
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Zero Trust"), "{err}");
    }

    /// Renewing refreshes the same service token (the devices keep their
    /// key) and reads the new date; a token deleted in Cloudflare is said
    /// plainly.
    #[tokio::test]
    async fn renewing_refreshes_the_devices_key() {
        let (base, calls) = fake_cloudflare("never").await;
        let cf = client(base);
        let rec = cf
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap();
        calls.lock().clear();
        let until = cf.renew(&rec).await.unwrap();
        assert_eq!(until, 1_822_867_200, "2027-10-07");
        let made = calls.lock().clone();
        assert!(
            made.iter()
                .any(|c| c == "POST /accounts/acct1/access/service_tokens/st1/refresh"),
            "{made:#?}"
        );
        assert!(
            !made
                .iter()
                .any(|c| c.starts_with("DELETE") || c.ends_with("service_tokens")),
            "no new key: {made:#?}"
        );
        let (base, _) =
            fake_cloudflare("404 POST /accounts/acct1/access/service_tokens/st1/refresh").await;
        let err = client(base).renew(&rec).await.unwrap_err().to_string();
        assert!(err.contains("no longer has the key"), "{err}");
    }

    /// Browsers get in at `/web` only: one more Access application, for that
    /// path, letting everyone through (Tessera checks them itself); the
    /// rest stays locked. Made once, and removed with the rest.
    #[tokio::test]
    async fn the_web_door_opens_only_web_and_goes_with_the_rest() {
        let (base, calls) = fake_cloudflare("never").await;
        let cf = client(base);
        let mut rec = cf
            .setup(
                &zone(),
                "tessera.example.com",
                "http://127.0.0.1:7878",
                &|_| {},
            )
            .await
            .unwrap();
        assert_eq!(rec.web_app_id, "", "not made with the tunnel");
        calls.lock().clear();
        rec.web_app_id = cf.web_door(&rec).await.unwrap();
        assert_eq!(rec.web_app_id, "app2");
        let made = calls.lock().clone();
        let body: Value = made
            .iter()
            .find_map(|c| c.strip_prefix("BODY "))
            .map(|b| serde_json::from_str(b).unwrap())
            .expect("an application");
        assert_eq!(body["domain"], "tessera.example.com/web");
        assert_eq!(body["policies"][0]["decision"], "bypass");
        assert_eq!(body["policies"].as_array().unwrap().len(), 1);
        assert!(
            !made
                .iter()
                .any(|c| c.starts_with("DELETE") || c.starts_with("PUT"))
        );
        // Removing remote access removes it too.
        calls.lock().clear();
        cf.remove(&rec).await.unwrap();
        let gone = calls.lock().clone();
        for want in [
            "DELETE /accounts/acct1/access/apps/app2",
            "DELETE /accounts/acct1/access/apps/app1",
            "DELETE /accounts/acct1/cfd_tunnel/tun1",
        ] {
            assert!(gone.iter().any(|c| c == want), "missing {want}: {gone:#?}");
        }
    }
}
