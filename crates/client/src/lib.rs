//! Connection to a Tessera server: race a server's addresses
//! through health and authenticated login, prefer the last successful one,
//! and reconnect in the background. Saved order breaks ties; proxy headers
//! go only to remote HTTPS addresses, never a home-network host.
//!
//! Paired devices talk to the server end-to-end encrypted (SPAKE2 pairing,
//! then Noise sessions against the pinned server key; see `tessera-secure`),
//! so proxies and tunnels in between only relay ciphertext. A config with a
//! pinned key never falls back to plaintext.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tessera_proto::e2e::{
    PairFinish, PairFinished, PairGrant, PairHello, PairStart, PairStarted, SessionAuth,
    SessionAuthReply,
};
use tessera_proto::{ApiError, ClientMessage, ErrorCode, Event, Request, ServerMessage};
use tessera_secure::{Opener, Sealer};
/// Which computer this is (`DeviceHello`), for the apps.
pub use tessera_secure::{computer_key, host_name};
use tokio::sync::{broadcast, oneshot, watch};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http::HeaderValue};

mod controls;
use controls::Controls;
mod outbox;
mod transport;
use outbox::{Lease, Outbox};
use transport::{ProgressIo, WriteProgress};

type Transport = tokio_tungstenite::MaybeTlsStream<ProgressIo<tokio::net::TcpStream>>;
type Socket = tokio_tungstenite::WebSocketStream<Transport>;
type Connected<S> = (tokio_tungstenite::WebSocketStream<S>, Link, WriteProgress);
type ConnectResult<S> = Result<(String, Connected<S>), (Option<String>, Failure)>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub token: String,
    /// The server's public key, pinned when pairing (base64). When set, every
    /// connection is end-to-end encrypted and authenticated against it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server_key: String,
}

/// What the server last said about how to reach it (`get_server_route`),
/// kept with a saved server: the next answer then tells which of the saved
/// addresses and headers came from the server (and may go) and which the
/// user added (and stay).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KnownRoute {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub header_names: Vec<String>,
    /// Values we installed, so a later user edit is never overwritten. `None`
    /// is a legacy route whose header ownership is unknown: preserve it.
    #[serde(default)]
    pub header_values: Option<BTreeMap<String, String>>,
    /// Addresses added by route-follow, separate from addresses the user
    /// already saved. Legacy routes only kept `urls`.
    #[serde(default)]
    pub managed_urls: Option<Vec<String>>,
}

impl KnownRoute {
    /// QR contents are server-supplied, unlike addresses/headers typed by the
    /// user. Save this ownership at import, before the first route refresh.
    pub fn from_pair_link(link: &tessera_proto::PairLink) -> Self {
        let urls: Vec<_> = link.urls.iter().map(|u| normalize(u)).collect();
        let headers: BTreeMap<_, _> = link.headers.iter().cloned().collect();
        Self {
            managed_urls: Some(urls.clone()),
            urls,
            header_names: headers.keys().cloned().collect(),
            header_values: Some(headers),
        }
    }
}

/// A saved server's addresses, headers and [`KnownRoute`] after the server
/// said how its devices reach it now (`route`), or `None` when nothing
/// changes. Keep saved addresses in their order, append newly learned ones,
/// and remove retired server addresses. Only headers we learned and the user
/// has not since edited can be replaced or removed. Loopback-only servers
/// stay local. Never leaves a server without an address; ignores invalid
/// addresses and headers.
pub fn merge_route(
    endpoints: &[String],
    headers: &BTreeMap<String, String>,
    known: Option<&KnownRoute>,
    route: &tessera_proto::ServerRoute,
) -> Option<(Vec<String>, BTreeMap<String, String>, KnownRoute)> {
    if !endpoints.is_empty()
        && endpoints
            .iter()
            .all(|e| is_loopback_endpoint(&normalize(e)))
    {
        return None;
    }
    let mut urls: Vec<String> = Vec::new();
    for u in &route.urls {
        let u = u.trim().trim_end_matches('/');
        let ok = (u.starts_with("http://") || u.starts_with("https://"))
            && u.len() <= 2048
            && url::Url::parse(u).is_ok_and(|p| p.host_str().is_some());
        if ok && !urls.iter().any(|x| x == u) {
            urls.push(u.to_string());
        }
    }
    let given: BTreeMap<String, String> = route
        .headers
        .iter()
        .filter(|(k, v)| {
            k.len() <= 256
                && v.len() <= 4096
                && !reserved_header(k)
                && tungstenite::http::HeaderName::from_bytes(k.as_bytes()).is_ok()
                && HeaderValue::from_str(v).is_ok()
        })
        .take(32)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let stale = |e: &str| {
        known.is_some_and(|k| {
            k.managed_urls
                .as_ref()
                .is_some_and(|managed| managed.iter().any(|u| normalize(u) == e))
        }) && !urls.iter().any(|u| u == e)
    };
    let mut next = Vec::new();
    for e in endpoints.iter().map(|e| normalize(e)).filter(|e| !stale(e)) {
        if !next.contains(&e) {
            next.push(e);
        }
    }
    for u in &urls {
        if !next.contains(u) {
            next.push(u.clone());
        }
    }
    if next.is_empty() {
        next = endpoints.to_vec();
    }
    let mut next_headers = headers.clone();
    let mut managed = BTreeMap::new();
    if let Some(previous) = known.and_then(|k| k.header_values.as_ref()) {
        for (k, v) in previous {
            let saved = next_headers
                .keys()
                .find(|s| s.eq_ignore_ascii_case(k))
                .cloned();
            if let Some(saved) = saved
                && next_headers.get(&saved) == Some(v)
            {
                next_headers.remove(&saved);
            }
        }
    }
    for (k, v) in &given {
        if !next_headers.keys().any(|s| s.eq_ignore_ascii_case(k)) {
            next_headers.insert(k.clone(), v.clone());
            managed.insert(k.clone(), v.clone());
        }
    }
    let managed_urls = urls
        .iter()
        .filter(|u| {
            !endpoints.iter().any(|e| normalize(e) == **u)
                || known.is_some_and(|k| {
                    k.managed_urls
                        .as_ref()
                        .is_some_and(|managed| managed.contains(u))
                })
        })
        .cloned()
        .collect();
    let next_known = KnownRoute {
        urls,
        header_names: managed.keys().cloned().collect(),
        header_values: Some(managed),
        managed_urls: Some(managed_urls),
    };
    let same = next == endpoints && &next_headers == headers && known == Some(&next_known);
    (!same).then_some((next, next_headers, next_known))
}

/// Headers the connection itself sets: a proxy header from the server never
/// replaces them.
fn reserved_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "host"
            | "connection"
            | "upgrade"
            | "authorization"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "keep-alive"
            | "proxy-connection"
    ) || n.starts_with("sec-websocket-")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    /// No endpoint reachable; retrying in the background.
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    pub state: ConnState,
    pub endpoint: Option<String>,
    pub error: Option<String>,
    /// The live connection is end-to-end encrypted to the pinned server key.
    #[serde(default)]
    pub encrypted: bool,
}

impl ConnectionStatus {
    fn new(state: ConnState, endpoint: Option<String>, error: Option<String>) -> Self {
        Self {
            state,
            endpoint,
            error,
            encrypted: false,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // short-lived; boxing every event isn't worth it
pub enum ClientEvent {
    Event(Event),
    Status(ConnectionStatus),
    /// A (re)connection completed: resynchronise open sessions.
    Connected,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(20))
        .redirect(same_host_redirects())
        .build()
        .unwrap_or_default()
}

/// Follow a redirect only on the same host (say, http → https): the custom
/// headers (Cloudflare Access secrets) must never be sent anywhere else, and
/// reqwest keeps them across hosts.
fn same_host_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|a| {
        let (first, next) = (&a.previous()[0], a.url());
        let upgrade = first.scheme() == "http" && next.scheme() == "https";
        let same = next.host_str() == first.host_str()
            && (next.port_or_known_default() == first.port_or_known_default() || upgrade)
            && (first.scheme() == next.scheme() || upgrade);
        if same && a.previous().len() < 5 {
            a.follow()
        } else {
            a.stop()
        }
    })
}

fn normalize(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.starts_with("http://") || e.starts_with("https://") {
        e.to_string()
    } else {
        format!("http://{e}")
    }
}

/// Proxy secrets go only to remote https addresses, never to a home address
/// (including a home server using TLS) that may belong to someone else on
/// the phone's current network.
fn headers_for<'a>(
    endpoint: &str,
    headers: &'a BTreeMap<String, String>,
) -> &'a BTreeMap<String, String> {
    static NONE: BTreeMap<String, String> = BTreeMap::new();
    if url::Url::parse(&normalize(endpoint))
        .is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| !is_local_host(h)))
    {
        headers
    } else {
        &NONE
    }
}

fn is_local_host(host: &str) -> bool {
    let host = host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || !host.contains('.') && !host.contains(':')
        || [".localhost", ".local", ".lan", ".home.arpa"]
            .iter()
            .any(|s| host.ends_with(s))
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v| is_local_host(&v.to_string()))
        }
        Err(_) => false,
    }
}

fn apply_headers(
    mut rb: reqwest::RequestBuilder,
    endpoint: &str,
    headers: &BTreeMap<String, String>,
) -> reqwest::RequestBuilder {
    for (k, v) in headers_for(endpoint, headers) {
        rb = rb.header(k, v);
    }
    rb
}

/// Returns the first endpoint (in preference order) whose health check
/// answers. All are probed concurrently; an earlier endpoint wins if it
/// answers within a short grace period.
pub async fn probe(endpoints: &[String], headers: &BTreeMap<String, String>) -> Result<String> {
    if endpoints.is_empty() {
        bail!("no endpoints configured");
    }
    let http = http_client();
    let checks = endpoints.iter().enumerate().map(|(i, e)| {
        let url = format!("{}/api/health", normalize(e));
        let rb = apply_headers(http.get(url).timeout(Duration::from_secs(5)), e, headers);
        async move {
            let r = rb.send().await.map_err(|e| unreachable_reason(&e))?;
            if !r.status().is_success() {
                return Err(status_reason(r.status().as_u16()));
            }
            let v: Value = r.json().await.map_err(|e| e.to_string())?;
            if v.get("name").and_then(Value::as_str) != Some("tessera") {
                return Err("not a Tessera server".to_string());
            }
            Ok::<usize, String>(i)
        }
    });
    match race_preferred(checks.collect(), Duration::from_millis(400)).await {
        Ok(i) => Ok(normalize(&endpoints[i])),
        Err(errors) => {
            let errors: Vec<String> = endpoints
                .iter()
                .zip(errors)
                .map(|(e, err)| format!("{e}: {err}"))
                .collect();
            bail!("couldn't reach the server ({})", errors.join("; "))
        }
    }
}

/// Why an address didn't answer, in words for the owner.
fn unreachable_reason(e: &reqwest::Error) -> String {
    let text = format!("{e:?}").to_ascii_lowercase();
    if text.contains("dns") || text.contains("failed to lookup") || text.contains("name or service")
    {
        "address not found".into()
    } else if e.is_timeout() || e.is_connect() {
        "no answer (not reachable from this network)".into()
    } else {
        e.to_string()
    }
}

/// What an HTTP error from a proxy in front of the server means.
fn status_reason(code: u16) -> String {
    match code {
        530 => "the tunnel isn't connected (Cloudflare can't reach the server)".into(),
        401 | 403 => {
            "refused by the proxy in front of it (its access headers are missing or wrong)".into()
        }
        502 | 504 => "the proxy in front of it can't reach the server".into(),
        _ => format!("HTTP {code}"),
    }
}

/// Whether the server takes requests that come through `url` as made on its
/// own computer: a reverse proxy there that doesn't say who connects (with
/// X-Forwarded-For) makes everyone who reaches it look local. `None`: it
/// couldn't be checked (unreachable, refused, or a server from before).
pub async fn seen_as_local(url: &str, headers: &BTreeMap<String, String>) -> Option<bool> {
    let health = format!("{}/api/health", normalize(url));
    let r = apply_headers(
        http_client().get(health).timeout(Duration::from_secs(8)),
        url,
        headers,
    )
    .send()
    .await
    .ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().await.ok()?;
    if v.get("name").and_then(Value::as_str) != Some("tessera") {
        return None;
    }
    v.get("local").and_then(Value::as_bool)
}

/// Runs the checks concurrently and returns the index of the most-preferred
/// (lowest index) one that succeeds. Once any check succeeds, more-preferred
/// ones still running get `grace` to finish; slow failures are never awaited
/// longer than that. Returns every error if none succeed.
async fn race_preferred<F>(
    checks: Vec<F>,
    grace: Duration,
) -> std::result::Result<usize, Vec<String>>
where
    F: std::future::Future<Output = std::result::Result<usize, String>>,
{
    race_ready(checks, grace).await.map(|(i, _)| i)
}

/// Race complete attempts, retaining only the best ready result and dropping
/// losers (including their sockets). A responsive health endpoint is not yet
/// a usable connection: the upgrade and pinned-key login must finish too.
async fn race_ready<F, T, E>(
    checks: Vec<F>,
    grace: Duration,
) -> std::result::Result<(usize, T), Vec<E>>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
{
    let n = checks.len();
    let mut running: futures::stream::FuturesUnordered<_> = checks
        .into_iter()
        .enumerate()
        .map(|(i, f)| async move { (i, f.await) })
        .collect();
    let mut errors: Vec<Option<E>> = std::iter::repeat_with(|| None).take(n).collect();
    let mut best: Option<(usize, T)> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        if let Some((b, _)) = &best
            && errors[..*b].iter().all(Option::is_some)
        {
            break;
        }
        let next = match deadline {
            Some(d) => match tokio::time::timeout_at(d, running.next()).await {
                Ok(next) => next,
                Err(_) => break,
            },
            None => running.next().await,
        };
        let Some((i, result)) = next else { break };
        match result {
            Ok(value) => {
                if best.as_ref().is_none_or(|(b, _)| i < *b) {
                    best = Some((i, value));
                }
                deadline.get_or_insert_with(|| tokio::time::Instant::now() + grace);
            }
            Err(e) => errors[i] = Some(e),
        }
    }
    best.ok_or_else(|| errors.into_iter().flatten().collect())
}

fn http_error(status: reqwest::StatusCode, body: &Value, fallback: &str) -> anyhow::Error {
    let msg = body
        .get("error")
        .and_then(|e| e.get("message").and_then(Value::as_str).or(e.as_str()))
        .map(str::to_string);
    anyhow!(
        "{}",
        msg.unwrap_or_else(|| format!("{fallback} (HTTP {status})"))
    )
}

async fn post_json<Req: Serialize, Resp: serde::de::DeserializeOwned>(
    endpoint: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &Req,
) -> Result<Resp> {
    let resp = apply_headers(
        http_client()
            .post(format!("{endpoint}{path}"))
            .timeout(Duration::from_secs(8)),
        endpoint,
        headers,
    )
    .json(body)
    .send()
    .await
    .context("pairing request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status == reqwest::StatusCode::NOT_FOUND {
            bail!("this server is too old for encrypted pairing; update it first");
        }
        return Err(http_error(status, &body, "pairing failed"));
    }
    resp.json().await.context("invalid pairing response")
}

/// Pair with a server using a pairing code, end-to-end encrypted: the code
/// keys a SPAKE2 exchange, and the device token and server key come back
/// sealed under it, so nothing in between can read or replace them. Returns
/// the endpoint used and the grant (store `token` and `server_key`).
pub async fn pair(
    endpoints: &[String],
    headers: &BTreeMap<String, String>,
    code: &str,
    device_name: &str,
) -> Result<(String, PairGrant)> {
    pair_as(endpoints, headers, code, device_name, false).await
}

/// [`pair`], as one of the user's devices or (`machine`) as a machine whose
/// login can only serve as that machine (`PairGrant::machine` says whether
/// the server made it one; servers from before don't).
pub async fn pair_as(
    endpoints: &[String],
    headers: &BTreeMap<String, String>,
    code: &str,
    device_name: &str,
    machine: bool,
) -> Result<(String, PairGrant)> {
    let digits = tessera_secure::pairing::normalize_code(code);
    if digits.len() != 8 {
        bail!("pairing codes have 8 digits");
    }
    let winner = probe(endpoints, headers).await?;
    let mut attempts = vec![winner];
    for e in endpoints {
        let e = normalize(e);
        if !attempts.contains(&e) {
            attempts.push(e);
        }
    }
    // Only pre-finish failures are safe to retry. Starting PAKE does not
    // consume the code; sending a finish may create a login even if its
    // answer is lost. Bound both each attempt and the whole fallback phase.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut errors = Vec::new();
    for endpoint in attempts.into_iter().take(8) {
        let start = async {
            let (pake, msg) = tessera_secure::pairing::DevicePake::start(&digits);
            let started: PairStarted = post_json(
                &endpoint,
                "/api/pair/start",
                headers,
                &PairStart {
                    pake: tessera_secure::b64(&msg),
                },
            )
            .await?;
            let keys = pake
                .finish(&tessera_secure::unb64(&started.pake)?)
                .context("the server's pairing reply was invalid")?;
            anyhow::Ok((started, keys))
        };
        let attempt_end = deadline.min(tokio::time::Instant::now() + Duration::from_secs(8));
        let (started, keys) = match tokio::time::timeout_at(attempt_end, start).await {
            Ok(Ok(v)) => v,
            failed => {
                let reason = match failed {
                    Ok(Err(e)) => e.to_string(),
                    _ => "pairing start timed out".into(),
                };
                errors.push(format!("{endpoint}: {reason}"));
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                continue;
            }
        };
        let hello = serde_json::to_vec(&PairHello {
            device_name: device_name.to_string(),
            machine,
            device_key: None,
        })?;
        let finished: PairFinished = post_json(
            &endpoint, "/api/pair/finish", headers,
            &PairFinish { pairing: started.pairing, sealed: tessera_secure::b64(&keys.seal_to_server(&hello)) },
        ).await.map_err(|e| anyhow!("{e}; pairing finish may have completed on the server, so no other address was retried. If needed, make a new pairing code and pair again"))?;
        let grant = keys.open_from_server(&tessera_secure::unb64(&finished.sealed)?)?;
        let grant: PairGrant = serde_json::from_slice(&grant).context("invalid pairing grant")?;
        if tessera_secure::unb64(&grant.server_key)
            .map(|k| k.len())
            .ok()
            != Some(32)
        {
            bail!("the server sent an invalid key");
        }
        return Ok((endpoint, grant));
    }
    bail!("couldn't start encrypted pairing ({})", errors.join("; "))
}

/// Pair from a `tessera://pair` link (scanned from a QR code): its addresses,
/// proxy headers and code, and a check that the server's key matches the
/// fingerprint the link carries, so a man in the middle can't slip in.
pub async fn pair_with_link(
    link: &str,
    device_name: &str,
) -> Result<(tessera_proto::PairLink, String, PairGrant)> {
    pair_with_link_as(link, device_name, false).await
}

/// [`pair_with_link`], as a device or (`machine`) as a machine.
pub async fn pair_with_link_as(
    link: &str,
    device_name: &str,
    machine: bool,
) -> Result<(tessera_proto::PairLink, String, PairGrant)> {
    let link = tessera_proto::PairLink::parse(link).map_err(|e| anyhow!(e))?;
    let headers: BTreeMap<String, String> = link.headers.iter().cloned().collect();
    let (endpoint, grant) = pair_as(&link.urls, &headers, &link.code, device_name, machine).await?;
    if let Some(expected) = &link.fingerprint
        && key_fingerprint(&grant.server_key).as_deref() != Some(expected.as_str())
    {
        bail!(
            "the server's key doesn't match the QR code; something may be intercepting the connection, so nothing was saved"
        );
    }
    Ok((link, endpoint, grant))
}

/// Trade one of the user's device logins for a login for that device's
/// machine side (`machine_login`): what `tessera node` keeps, so its file
/// on that computer can never act as the device. `config` comes back with
/// the machine's token; the device's own login keeps working.
pub async fn machine_login(config: &ServerConfig) -> Result<ServerConfig> {
    let client = Client::connect(config.clone());
    let got = async {
        client.wait_connected(Duration::from_secs(15)).await?;
        let grant: tessera_proto::MachineLoginGrant = client
            .call_timeout(Request::MachineLogin, Duration::from_secs(15))
            .await
            .and_then(|v| {
                serde_json::from_value(v).map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("unexpected answer: {e}"))
                })
            })
            .map_err(|e| anyhow!(e.message))?;
        anyhow::Ok(grant)
    }
    .await;
    client.close();
    let grant = got?;
    Ok(ServerConfig {
        token: grant.token,
        ..config.clone()
    })
}

/// The endpoint is this machine (`localhost`, `127.0.0.1`, `[::1]`), where
/// nothing sits between client and server.
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host().map(|h| h.to_owned()))
        .is_some_and(|h| match h {
            url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        })
}

/// Fingerprint of a pinned server key, for showing to the user.
pub fn key_fingerprint(server_key: &str) -> Option<String> {
    tessera_secure::unb64(server_key)
        .ok()
        .filter(|k| k.len() == 32)
        .map(|k| tessera_secure::fingerprint(&k))
}

type Waiters = Arc<Mutex<std::collections::HashMap<u64, oneshot::Sender<Result<Value, ApiError>>>>>;

/// A managed, auto-reconnecting connection.
#[derive(Clone)]
pub struct Client {
    /// Where and how it connects; [`Client::set_route`] changes it for the
    /// next connection.
    config: Arc<Mutex<ServerConfig>>,
    outbound: Arc<Outbox>,
    waiters: Waiters,
    next_id: Arc<AtomicU64>,
    transport_epoch: Arc<AtomicU64>,
    instance_id: u64,
    events: broadcast::Sender<ClientEvent>,
    status: watch::Receiver<ConnectionStatus>,
    stop: Arc<tokio_util_lite::Stop>,
    nudge: Arc<tokio::sync::Notify>,
    // Only Client handles own this Arc; the manager must never retain it.
    _lifetime: Arc<ClientLifetime>,
}

struct ClientLifetime {
    outbound: Arc<Outbox>,
    stop: Arc<tokio_util_lite::Stop>,
}

impl Drop for ClientLifetime {
    fn drop(&mut self) {
        self.stop.stop();
        self.outbound.close();
    }
}

/// RPCs are useful only while their caller still waits. Posts deliberately
/// have no expiry and may wait through reconnects (but are never replayed once sent).
#[cfg(test)]
struct QueuedRequest {
    id: u64,
    request: Request,
    expires: Option<tokio::time::Instant>,
    epoch: Option<u64>,
}

#[cfg(test)]
impl QueuedRequest {
    fn into_message_in_epoch(self, waiters: &Waiters, epoch: u64) -> Option<ClientMessage> {
        if self.epoch.is_some_and(|bound| bound != epoch) {
            return None;
        }
        self.into_message(waiters)
    }

    fn into_message(self, waiters: &Waiters) -> Option<ClientMessage> {
        if let Some(expires) = self.expires
            && (tokio::time::Instant::now() >= expires
                || !waiters
                    .lock()
                    .get(&self.id)
                    .is_some_and(|tx| !tx.is_closed()))
        {
            return None;
        }
        Some(ClientMessage {
            id: self.id,
            request: self.request,
        })
    }
}

/// Also removes a waiter when the call future itself is dropped/aborted.
struct WaitingCall<'a> {
    id: u64,
    waiters: &'a Waiters,
    outbound: &'a Outbox,
    lease: Arc<Lease>,
    cancel_on_drop: bool,
}

impl Drop for WaitingCall<'_> {
    fn drop(&mut self) {
        self.waiters.lock().remove(&self.id);
        if self.cancel_on_drop {
            self.lease.cancelled.stop();
        }
        self.outbound.cancel(self.id);
    }
}
mod tokio_util_lite {
    /// Minimal cancellation flag (avoids a tokio-util dependency here).
    #[derive(Default)]
    pub struct Stop {
        flag: std::sync::atomic::AtomicBool,
        notify: tokio::sync::Notify,
    }
    impl Stop {
        pub fn stop(&self) {
            self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_waiters();
        }
        pub fn stopped(&self) -> bool {
            self.flag.load(std::sync::atomic::Ordering::SeqCst)
        }
        pub async fn wait(&self) {
            // notify_waiters remembers notifications since this future was
            // created, including a Stop between the flag check and the await.
            let notified = self.notify.notified();
            if self.stopped() {
                return;
            }
            notified.await
        }
    }
}

impl Client {
    /// Start a managed connection. Must be called within a Tokio runtime.
    pub fn connect(config: ServerConfig) -> Self {
        Self::connect_preferred(config, None)
    }

    /// Start with the address that last completed a login, without changing
    /// the user's saved order. A stale preference never delays a working one.
    pub fn connect_preferred(mut config: ServerConfig, last_endpoint: Option<String>) -> Self {
        prefer_last(&mut config.endpoints, last_endpoint.as_deref());
        let outbound = Arc::new(Outbox::default());
        let (events, _) = broadcast::channel(4096);
        let (status_tx, status_rx) =
            watch::channel(ConnectionStatus::new(ConnState::Connecting, None, None));
        let waiters: Waiters = Arc::default();
        let stop = Arc::new(tokio_util_lite::Stop::default());
        let nudge = Arc::new(tokio::sync::Notify::new());
        let config = Arc::new(Mutex::new(config));
        let transport_epoch = Arc::new(AtomicU64::new(0));
        static INSTANCE: AtomicU64 = AtomicU64::new(1);
        let instance_id = INSTANCE.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(manager(
            config.clone(),
            outbound.clone(),
            waiters.clone(),
            events.clone(),
            status_tx,
            stop.clone(),
            nudge.clone(),
            transport_epoch.clone(),
        ));
        Self {
            _lifetime: Arc::new(ClientLifetime {
                outbound: outbound.clone(),
                stop: stop.clone(),
            }),
            config,
            outbound,
            waiters,
            next_id: Arc::new(AtomicU64::new(1)),
            transport_epoch,
            instance_id,
            events,
            status: status_rx,
            stop,
            nudge,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status.borrow().clone()
    }

    /// Wait (up to `timeout`) for a live connection.
    pub async fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let mut rx = self.status.clone();
        let fut = async {
            loop {
                if rx.borrow().state == ConnState::Connected {
                    return Ok(());
                }
                if rx.changed().await.is_err() {
                    return Err(anyhow!("connection closed"));
                }
            }
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(anyhow!(
                "could not connect: {}",
                self.status().error.unwrap_or_else(|| "timed out".into())
            )),
        }
    }

    /// Send a request and await its result. Requests made while disconnected
    /// wait for the connection (bounded by `timeout`).
    pub async fn call(&self, req: Request) -> Result<Value, ApiError> {
        self.call_timeout(req, Duration::from_secs(120)).await
    }

    pub async fn call_timeout(&self, req: Request, timeout: Duration) -> Result<Value, ApiError> {
        self.call_timeout_scoped(req, timeout, None).await
    }

    /// A transient request must never migrate to another transport.
    pub async fn call_timeout_in_epoch(
        &self,
        epoch: u64,
        req: Request,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        if self.connected_epoch() != Some(epoch) {
            return Err(ApiError::new(ErrorCode::Internal, "connection changed"));
        }
        self.call_timeout_scoped(req, timeout, Some(epoch)).await
    }

    async fn call_timeout_scoped(
        &self,
        req: Request,
        timeout: Duration,
        epoch: Option<u64>,
    ) -> Result<Value, ApiError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let request = async {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let mut payload = self.outbound.admit(id, req, deadline).await?;
            payload.epoch = epoch;
            let (tx, rx) = oneshot::channel();
            self.waiters.lock().insert(id, tx);
            let mut waiting = WaitingCall {
                id,
                waiters: &self.waiters,
                outbound: &self.outbound,
                lease: payload.lease.clone(),
                cancel_on_drop: true,
            };
            if let Some(epoch) = epoch {
                if self.connected_epoch() != Some(epoch) {
                    return Err(ApiError::new(ErrorCode::Internal, "connection changed"));
                }
            } else if self.status().state != ConnState::Connected {
                self.wait_connected(
                    deadline
                        .saturating_duration_since(tokio::time::Instant::now())
                        .min(Duration::from_secs(30)),
                )
                .await
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            }
            self.outbound.push(payload)?;
            match rx.await {
                Ok(result) => {
                    waiting.cancel_on_drop = false;
                    result
                }
                Err(_) => Err(ApiError::new(
                    ErrorCode::Internal,
                    "connection lost before the server answered",
                )),
            }
        };
        tokio::time::timeout_at(deadline, request)
            .await
            .unwrap_or_else(|_| Err(ApiError::new(ErrorCode::Internal, "request timed out")))
    }

    pub async fn call_as<T: serde::de::DeserializeOwned>(
        &self,
        req: Request,
    ) -> Result<T, ApiError> {
        let v = self.call(req).await?;
        serde_json::from_value(v)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("unexpected response: {e}")))
    }

    /// Fire-and-forget: queues through reconnects without a caller or expiry.
    /// Sent once, in order; no delivery guarantee and no retry after sending.
    /// Returns Busy if the 128-request / 16 MiB budget is full; never silently
    /// drops a request. Use call_timeout for timeout-bounded backpressure.
    pub fn post(&self, req: Request) -> Result<(), ApiError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.outbound.post(id, req)
    }

    /// Stable across clones/reconnects, different for every Client install.
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// The live transport token, not the lifetime of this saved Client.
    pub fn connected_epoch(&self) -> Option<u64> {
        (self.status().state == ConnState::Connected && !self.stop.stopped())
            .then(|| self.transport_epoch.load(Ordering::SeqCst))
    }

    /// Never carry a transient acknowledgement/activation through reconnect.
    pub fn post_in_epoch(&self, epoch: u64, req: Request) {
        if self.connected_epoch() != Some(epoch) {
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Historical API is best-effort, but still shares Outbox admission.
        let _ = self.outbound.post_in_epoch(id, req, Some(epoch));
    }

    pub fn close(&self) {
        self.outbound.close();
        self.stop.stop();
    }

    /// How the server says its devices reach it now (`get_server_route`):
    /// its addresses and the proxy headers they need. `None` from a server
    /// from before this, one that doesn't give it to this login, or when
    /// it doesn't answer in time.
    pub async fn server_route(&self) -> Option<tessera_proto::ServerRoute> {
        let v = self
            .call_timeout(Request::GetServerRoute, Duration::from_secs(15))
            .await
            .ok()?;
        serde_json::from_value(v).ok()
    }

    /// Use these addresses and headers from the next connection on (the
    /// live one stays). See [`merge_route`].
    pub fn set_route(&self, endpoints: Vec<String>, headers: BTreeMap<String, String>) {
        let mut c = self.config.lock();
        c.endpoints = endpoints;
        c.headers = headers;
    }

    /// Hint that the network may have changed (e.g. the app came back to the
    /// foreground): a live link is checked within seconds, and a client that
    /// is waiting to retry reconnects right away instead of sleeping.
    pub fn nudge(&self) {
        self.nudge.notify_one();
    }

    /// Whether a shell event still belongs to its current connection (not a
    /// replaced login to the same saved server).
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.config, &other.config)
    }
}

#[allow(clippy::too_many_arguments)]
async fn manager(
    config: Arc<Mutex<ServerConfig>>,
    outbound: Arc<Outbox>,
    waiters: Waiters,
    events: broadcast::Sender<ClientEvent>,
    status: watch::Sender<ConnectionStatus>,
    stop: Arc<tokio_util_lite::Stop>,
    nudge: Arc<tokio::sync::Notify>,
    transport_epoch: Arc<AtomicU64>,
) {
    manager_with_connect(
        config,
        outbound,
        waiters,
        events,
        status,
        stop,
        nudge,
        transport_epoch,
        |c| async move { connect_any(&c).await },
    )
    .await;
}

// Keep the actual queue/reconnect loop testable with a bounded byte transport.
#[allow(clippy::too_many_arguments)]
async fn manager_with_connect<S, F, Fut>(
    config: Arc<Mutex<ServerConfig>>,
    outbound: Arc<Outbox>,
    waiters: Waiters,
    events: broadcast::Sender<ClientEvent>,
    status: watch::Sender<ConnectionStatus>,
    stop: Arc<tokio_util_lite::Stop>,
    nudge: Arc<tokio::sync::Notify>,
    transport_epoch: Arc<AtomicU64>,
    connect: F,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: Fn(ServerConfig) -> Fut,
    Fut: std::future::Future<Output = ConnectResult<S>>,
{
    let mut backoff = Duration::from_millis(500);
    let mut last_endpoint: Option<String> = None;
    let set = |s: ConnectionStatus| {
        let _ = events.send(ClientEvent::Status(s.clone()));
        let _ = status.send(s);
    };
    // Outbox owns all unsent work through reconnects; it retains the same
    // admission leases until a post is sent or an RPC completes/cancels.
    while !stop.stopped() {
        set(ConnectionStatus::new(ConnState::Connecting, None, None));
        let mut current = config.lock().clone();
        prefer_last(&mut current.endpoints, last_endpoint.as_deref());
        let attempt = tokio::select! {
            result = connect(current) => result,
            _ = stop.wait() => break,
        };
        let (endpoint, ws) = match attempt {
            Ok(v) => v,
            Err((None, Failure { message, .. })) => {
                // Nothing answered.
                set(ConnectionStatus::new(
                    ConnState::Offline,
                    None,
                    Some(message),
                ));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = nudge.notified() => backoff = Duration::from_millis(250),
                    _ = stop.wait() => break,
                }
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
            Err((endpoint, Failure { message, fatal })) => {
                set(ConnectionStatus::new(
                    ConnState::Offline,
                    endpoint,
                    Some(message),
                ));
                if fatal {
                    // Keep retrying slowly in case the server was restored.
                    backoff = Duration::from_secs(30);
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = nudge.notified() => backoff = Duration::from_millis(250),
                    _ = stop.wait() => break,
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        let (ws, link, progress) = ws;
        last_endpoint = Some(endpoint.clone());
        backoff = Duration::from_millis(500);
        let encrypted = matches!(link, Link::Secure { .. });
        let epoch = transport_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        set(ConnectionStatus {
            state: ConnState::Connected,
            endpoint: Some(endpoint.clone()),
            error: None,
            encrypted,
        });
        let (mut sink, mut stream) = ws.split();
        let (mut writer, mut reader) = link.split();
        let ping = Controls::new();
        let writing = connection_writer(
            &mut writer,
            &mut sink,
            &outbound,
            &waiters,
            &stop,
            &progress,
            &ping,
            &events,
            epoch,
        );
        let reading = connection_reader(
            &mut reader,
            &mut stream,
            &waiters,
            &events,
            &stop,
            &nudge,
            &ping,
        );
        // Both are polled throughout every send. Ending either drops the other
        // and the partially sent socket; old framing/Noise state is never reused.
        let reason = tokio::select! {
            biased;
            _ = stop.wait() => None,
            reason = reading => reason,
            reason = writing => reason,
        };
        // Invalidate scoped messages before anything can be requeued.
        transport_epoch.fetch_add(1, Ordering::SeqCst);
        // Fail in-flight requests; the caller can retry after reconnecting.
        for (_, tx) in waiters.lock().drain() {
            let _ = tx.send(Err(ApiError::new(ErrorCode::Internal, "connection lost")));
        }

        if reason.as_deref().is_some_and(|r| {
            r == "connection lost while in the background"
                || r == "reconnecting after the app was paused"
        }) {
            backoff = Duration::ZERO;
        }
        match reason {
            None => break,
            Some(r) => set(ConnectionStatus::new(
                ConnState::Offline,
                Some(endpoint),
                Some(r),
            )),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = nudge.notified() => backoff = Duration::from_millis(250),
            _ = stop.wait() => break,
        }
    }
    outbound.close();
    for (_, tx) in waiters.lock().drain() {
        let _ = tx.send(Err(ApiError::new(ErrorCode::Internal, "client stopped")));
    }
    set(ConnectionStatus::new(ConnState::Disconnected, None, None));
}

#[allow(clippy::too_many_arguments)]
async fn connection_writer<S>(
    writer: &mut WriteLink,
    sink: &mut S,
    outbound: &Outbox,
    waiters: &Waiters,
    stop: &tokio_util_lite::Stop,
    progress: &WriteProgress,
    ping: &Controls,
    events: &broadcast::Sender<ClientEvent>,
    epoch: u64,
) -> Option<String>
where
    S: futures::Sink<tungstenite::Message> + Unpin,
{
    let hello = ClientMessage {
        id: 0,
        request: Request::Hello {
            client: "tessera-client".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    };
    if !writer
        .send(
            sink,
            serde_json::to_string(&hello).expect("hello serializes"),
            stop,
            progress,
            ping,
        )
        .await
    {
        return Some("send failed".into());
    }
    let _ = events.send(ClientEvent::Connected);
    loop {
        tokio::select! {
            biased;
            _ = stop.wait() => return None,
            _ = ping.ready.notified() => {
                if !ping.flush(sink, stop, progress).await { return Some("control send failed".into()); }
            }
            queued = outbound.recv() => {
                let mut payload = queued?;
                if !payload.live_in_epoch(waiters, epoch) { continue; }
                let text = std::mem::take(&mut payload.text);
                // Retain the admission lease while the socket still owns the
                // encoded payload. Cancelling an active frame drops the socket,
                // rather than skipping a request on a half-written stream.
                let sent = tokio::select! {
                    biased;
                    _ = payload.lease.cancelled.wait() => false,
                    sent = writer.send(sink, text, stop, progress, ping) => sent,
                };
                if !sent { return Some("send failed".into()); }
            }
        }
    }
}

async fn connection_reader<S>(
    reader: &mut ReadLink,
    stream: &mut S,
    waiters: &Waiters,
    events: &broadcast::Sender<ClientEvent>,
    stop: &tokio_util_lite::Stop,
    nudge: &tokio::sync::Notify,
    ping_request: &Controls,
) -> Option<String>
where
    S: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>> + Unpin,
{
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    ping.tick().await;
    let mut last_seen = tokio::time::Instant::now();
    let mut probe: Option<tokio::time::Instant> = None;
    let mut flushed = ping_request.subscribe();
    let mut probe_waiting = false;
    loop {
        let deadline = probe
            .unwrap_or(last_seen + Duration::from_secs(40))
            .min(last_seen + Duration::from_secs(40));
        tokio::select! {
            biased;
            _ = stop.wait() => return None,
            _ = tokio::time::sleep_until(deadline) => return Some(if probe.is_some() { "connection lost while in the background" } else { "connection stalled" }.into()),
            msg = stream.next() => {
                match msg {
                    Some(Ok(m @ (tungstenite::Message::Text(_) | tungstenite::Message::Binary(_)))) => match reader.receive(m) {
                        Ok(Some(text)) => handle_incoming(&text, waiters, events),
                        Ok(None) => {}
                        Err(e) => return Some(e),
                    },
                    Some(Ok(tungstenite::Message::Ping(bytes))) => ping_request.pong(bytes),
                    Some(Ok(tungstenite::Message::Close(_))) | None => return Some("server closed the connection".into()),
                    Some(Err(e)) => return Some(e.to_string()),
                    _ => {}
                }
                ping_request.observed_input();
                last_seen = tokio::time::Instant::now();
                probe = None;
                probe_waiting = false;
            }
            _ = flushed.changed() => {
                if let Some((sent, generation)) = *flushed.borrow_and_update()
                    && ping_request.read_generation() == generation
                {
                    probe.get_or_insert(sent + Duration::from_millis(2500));
                }
            }
            _ = ping.tick() => ping_request.ping(false),
            _ = nudge.notified() => {
                if last_seen.elapsed() > Duration::from_secs(20) { return Some("reconnecting after the app was paused".into()); }
                if probe.is_none() && !probe_waiting {
                    probe_waiting = true;
                    ping_request.ping(true);
                }
            }
        }
    }
}

fn prefer_last(endpoints: &mut [String], last: Option<&str>) {
    if let Some(i) = last.and_then(|last| {
        endpoints
            .iter()
            .position(|e| normalize(e) == normalize(last))
    }) {
        endpoints[..=i].rotate_right(1);
    }
}

/// Race health, upgrade and authenticated login together. A LAN server that
/// answers health but stalls at the WebSocket must not hold up mobile data.
async fn connect_any(config: &ServerConfig) -> ConnectResult<Transport> {
    let checks = config
        .endpoints
        .iter()
        .map(|e| async move {
            let endpoint = probe(std::slice::from_ref(e), &config.headers)
                .await
                .map_err(|err| (None, Failure::retry(err.to_string())))?;
            open(&endpoint, config)
                .await
                .map(|ws| (endpoint.clone(), ws))
                .map_err(|err| (Some(endpoint), err))
        })
        .collect();
    match race_ready(checks, Duration::from_millis(400)).await {
        Ok((_, ready)) => Ok(ready),
        Err(errors) => {
            // Unreachable health endpoints say nothing about whether this
            // login is valid. Preserve a definitive authentication rejection
            // if every address that could actually open rejected it.
            let mut opened = errors.iter().filter(|(ep, _)| ep.is_some()).peekable();
            let fatal = opened.peek().is_some() && opened.all(|(_, e)| e.fatal);
            let mut messages = Vec::new();
            let mut endpoint = None;
            for (ep, err) in errors {
                if endpoint.is_none() {
                    endpoint = ep;
                }
                messages.push(err.message);
            }
            Err((
                endpoint,
                Failure {
                    message: if messages.is_empty() {
                        "no endpoints configured".into()
                    } else {
                        messages.join("; ")
                    },
                    fatal,
                },
            ))
        }
    }
}

/// Why a connection attempt failed; `fatal` ones need the user (re-pairing).
struct Failure {
    message: String,
    fatal: bool,
}

impl Failure {
    fn retry(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
        }
    }
    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
        }
    }
}

/// How messages are framed on a live socket.
enum Link {
    /// JSON text frames (browser-compatible; the server allows it only locally
    /// unless `require_e2ee` is off).
    Plain,
    /// Every message sealed with the session keys, as binary frames.
    Secure { sealer: Sealer, opener: Opener },
}

/// End a send only after 40 seconds without bytes accepted by the transport,
/// or immediately on Close. Buffering/polling alone never renews the deadline.
async fn send_frame<S>(
    sink: &mut S,
    frame: tungstenite::Message,
    stop: &tokio_util_lite::Stop,
    progress: &WriteProgress,
) -> bool
where
    S: futures::Sink<tungstenite::Message> + Unpin,
{
    progress.begin();
    tokio::select! {
        biased;
        _ = stop.wait() => false,
        result = sink.send(frame) => result.is_ok(),
        _ = progress.stalled() => false,
    }
}
enum WriteLink {
    Plain,
    Secure(Sealer),
}
enum ReadLink {
    Plain,
    Secure(Opener),
}

impl Link {
    fn split(self) -> (WriteLink, ReadLink) {
        match self {
            Self::Plain => (WriteLink::Plain, ReadLink::Plain),
            Self::Secure { sealer, opener } => {
                (WriteLink::Secure(sealer), ReadLink::Secure(opener))
            }
        }
    }
}
impl WriteLink {
    async fn send<S>(
        &mut self,
        sink: &mut S,
        text: String,
        stop: &tokio_util_lite::Stop,
        progress: &WriteProgress,
        controls: &Controls,
    ) -> bool
    where
        S: futures::Sink<tungstenite::Message> + Unpin,
    {
        match self {
            WriteLink::Plain => {
                // RFC 6455 fragmentation lets controls pass between frames.
                use tungstenite::protocol::frame::{
                    Frame,
                    coding::{Data, OpCode},
                };
                let bytes: tungstenite::Bytes = text.into_bytes().into();
                const FRAME_BYTES: usize = 65_518;
                let len = bytes.len();
                for start in (0..len).step_by(FRAME_BYTES) {
                    if !controls.flush(sink, stop, progress).await {
                        return false;
                    }
                    let end = (start + FRAME_BYTES).min(len);
                    let opcode = if start == 0 {
                        Data::Text
                    } else {
                        Data::Continue
                    };
                    let frame =
                        Frame::message(bytes.slice(start..end), OpCode::Data(opcode), end == len);
                    if !send_frame(sink, tungstenite::Message::Frame(frame), stop, progress).await {
                        return false;
                    }
                }
                controls.flush(sink, stop, progress).await
            }
            WriteLink::Secure(sealer) => {
                for frame in sealer.seal_frames(text.as_bytes()) {
                    let Ok(f) = frame else {
                        return false;
                    };
                    if !controls.flush(sink, stop, progress).await {
                        return false;
                    }
                    if !send_frame(sink, tungstenite::Message::Binary(f.into()), stop, progress)
                        .await
                    {
                        return false;
                    }
                }
                controls.flush(sink, stop, progress).await
            }
        }
    }
}
impl ReadLink {
    /// A complete message, `None` while a chunked one is still arriving, or
    /// an error that must end the connection.
    fn receive(&mut self, msg: tungstenite::Message) -> Result<Option<String>, String> {
        match (self, msg) {
            (ReadLink::Plain, tungstenite::Message::Text(t)) => Ok(Some(t.to_string())),
            (ReadLink::Plain, tungstenite::Message::Binary(b)) => {
                Ok(Some(String::from_utf8_lossy(&b).to_string()))
            }
            (ReadLink::Secure(opener), tungstenite::Message::Binary(b)) => match opener.open(&b) {
                Ok(Some(m)) => String::from_utf8(m)
                    .map(Some)
                    .map_err(|_| "invalid message".to_string()),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("encrypted channel broken: {e}")),
            },
            (ReadLink::Secure(_), _) => {
                Err("the server sent an unencrypted message on an encrypted connection".into())
            }
            _ => Ok(None),
        }
    }
}

fn ws_request(
    endpoint: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
) -> Result<tungstenite::handshake::client::Request, Failure> {
    let url = format!(
        "{}{path}",
        endpoint
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| Failure::fatal(e.to_string()))?;
    for (k, v) in headers_for(endpoint, headers) {
        if let (Ok(name), Ok(value)) = (
            tungstenite::http::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            request.headers_mut().insert(name, value);
        }
    }
    Ok(request)
}

async fn upgrade(
    request: tungstenite::handshake::client::Request,
) -> Result<(Socket, WriteProgress), Failure> {
    let progress = WriteProgress::new();
    let connect = async {
        let host = request.uri().host().ok_or(tungstenite::Error::Url(
            tungstenite::error::UrlError::NoHostName,
        ))?;
        let port =
            request
                .uri()
                .port_u16()
                .unwrap_or(if request.uri().scheme_str() == Some("wss") {
                    443
                } else {
                    80
                });
        let tcp = tokio::net::TcpStream::connect((host.trim_matches(['[', ']']), port))
            .await
            .map_err(tungstenite::Error::Io)?;
        // Below TLS: buffered plaintext alone is not evidence of wire progress.
        tokio_tungstenite::client_async_tls_with_config(
            request,
            ProgressIo::new(tcp, progress.clone()),
            Some(
                tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(tessera_proto::MAX_SERIALIZED_RESPONSE_BYTES))
                    .max_frame_size(Some(tessera_proto::MAX_SERIALIZED_RESPONSE_BYTES)),
            ),
            None,
        )
        .await
    };
    match tokio::time::timeout(Duration::from_secs(10), connect).await {
        Ok(Ok((ws, _))) => Ok((ws, progress)),
        Ok(Err(tungstenite::Error::Http(r))) => {
            let body = r
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&body).ok().and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            Err(match r.status().as_u16() {
                401 => Failure::fatal(
                    "the server rejected this device's token (it may have been revoked); pair again",
                ),
                403 => Failure::fatal(detail.unwrap_or_else(|| {
                    "this server requires end-to-end encryption; pair this device again".into()
                })),
                404 => {
                    Failure::fatal("this server is too old for end-to-end encryption; update it")
                }
                s => Failure::retry(detail.unwrap_or_else(|| format!("HTTP {s}"))),
            })
        }
        Ok(Err(e)) => Err(Failure::retry(e.to_string())),
        Err(_) => Err(Failure::retry("connection timed out")),
    }
}

/// Open a socket to `endpoint`: encrypted when a server key is pinned,
/// otherwise the plain token socket.
async fn open(endpoint: &str, config: &ServerConfig) -> Result<Connected<Transport>, Failure> {
    if config.server_key.is_empty() {
        // Without a pinned key the token would travel readable: only to the
        // server on this computer.
        if !is_loopback_endpoint(endpoint) {
            return Err(Failure::fatal(
                "this login has no pinned server key, so it only works on the server's own computer; pair this device again",
            ));
        }
        let mut request = ws_request(endpoint, "/api/ws", &config.headers)?;
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", config.token)) {
            request.headers_mut().insert("authorization", v);
        }
        let (ws, progress) = upgrade(request).await?;
        return Ok((ws, Link::Plain, progress));
    }
    let key = tessera_secure::unb64(&config.server_key)
        .map_err(|_| Failure::fatal("the saved server key is corrupt; pair again"))?;
    let (mut ws, progress) = upgrade(ws_request(endpoint, "/api/secure", &config.headers)?).await?;
    let setup = async {
        let (initiator, first) =
            tessera_secure::Initiator::start(&key).map_err(|e| Failure::fatal(e.to_string()))?;
        ws.send(tungstenite::Message::Binary(first.into()))
            .await
            .map_err(|e| Failure::retry(e.to_string()))?;
        let reply = next_binary(&mut ws).await?;
        let channel = initiator
            .finish(&reply)
            .map_err(|e| Failure::fatal(e.to_string()))?;
        let (mut sealer, mut opener) = (channel.sealer, channel.opener);
        opener.set_limit(tessera_proto::MAX_SERIALIZED_RESPONSE_BYTES);
        let auth = serde_json::to_vec(&SessionAuth {
            token: config.token.clone(),
            proof: None,
        })
        .map_err(|e| Failure::fatal(e.to_string()))?;
        for f in sealer
            .seal(&auth)
            .map_err(|e| Failure::fatal(e.to_string()))?
        {
            ws.send(tungstenite::Message::Binary(f.into()))
                .await
                .map_err(|e| Failure::retry(e.to_string()))?;
        }
        let answer = loop {
            let frame = next_binary(&mut ws).await?;
            if let Some(m) = opener
                .open(&frame)
                .map_err(|e| Failure::fatal(e.to_string()))?
            {
                break m;
            }
        };
        let answer: SessionAuthReply = serde_json::from_slice(&answer)
            .map_err(|_| Failure::retry("invalid answer from the server"))?;
        if !answer.ok {
            return Err(Failure::fatal(
                answer
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "the server refused this device; pair again".into()),
            ));
        }
        Ok(Link::Secure { sealer, opener })
    };
    match tokio::time::timeout(Duration::from_secs(15), setup).await {
        Ok(Ok(link)) => Ok((ws, link, progress)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Failure::retry("encrypted handshake timed out")),
    }
}

async fn next_binary(ws: &mut Socket) -> Result<Vec<u8>, Failure> {
    loop {
        match ws.next().await {
            Some(Ok(tungstenite::Message::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_))) => continue,
            Some(Ok(tungstenite::Message::Close(Some(f)))) if u16::from(f.code) == 4001 => {
                return Err(Failure::fatal(
                    "the server failed to prove its identity: its key doesn't match the one saved when pairing (the server was reset, or something is intercepting the connection); pair again",
                ));
            }
            Some(Ok(_)) | None => {
                return Err(Failure::retry(
                    "the server closed the connection during the handshake",
                ));
            }
            Some(Err(e)) => return Err(Failure::retry(e.to_string())),
        }
    }
}

fn handle_incoming(text: &str, waiters: &Waiters, events: &broadcast::Sender<ClientEvent>) {
    let msg = match serde_json::from_str::<ServerMessage>(text) {
        Ok(msg) => msg,
        Err(e) => {
            // A malformed reply can still contain private chat text or keys.
            tracing::warn!(
                bytes = text.len(),
                category = ?e.classify(),
                line = e.line(),
                column = e.column(),
                "unparseable server message"
            );
            return;
        }
    };
    match msg {
        ServerMessage::Response { id, result } => {
            if let Some(tx) = waiters.lock().remove(&id) {
                let _ = tx.send(Ok(result));
            }
        }
        ServerMessage::Failure { id, error } => {
            if let Some(tx) = waiters.lock().remove(&id) {
                let _ = tx.send(Err(error));
            }
        }
        ServerMessage::Event { event } => {
            let _ = events.send(ClientEvent::Event(event));
        }
    }
}

#[cfg(test)]
mod lifetime_tests;
#[cfg(test)]
mod outbox_tests;
#[cfg(test)]
mod transport_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn stalled_sink()
    -> impl futures::Sink<tungstenite::Message, Error = std::convert::Infallible> + Unpin {
        Box::pin(futures::sink::unfold((), |(), _| {
            std::future::pending::<Result<(), std::convert::Infallible>>()
        }))
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_write_has_a_deadline() {
        let start = tokio::time::Instant::now();
        let stop = tokio_util_lite::Stop::default();
        assert!(
            !send_frame(
                &mut stalled_sink(),
                tungstenite::Message::Ping(Vec::new().into()),
                &stop,
                &WriteProgress::new(),
            )
            .await
        );
        assert_eq!(start.elapsed(), Duration::from_secs(40));
    }

    #[tokio::test(start_paused = true)]
    async fn close_interrupts_a_stalled_write() {
        let stop = tokio_util_lite::Stop::default();
        let progress = WriteProgress::new();
        let start = tokio::time::Instant::now();
        let end = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            stop.stop();
        };
        let mut sink = stalled_sink();
        let (sent, ()) = tokio::join!(
            send_frame(
                &mut sink,
                tungstenite::Message::Ping(Vec::new().into()),
                &stop,
                &progress,
            ),
            end
        );
        assert!(!sent);
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn writes_work_until_closed() {
        let stop = tokio_util_lite::Stop::default();
        let mut sink = futures::sink::drain();
        assert!(
            send_frame(
                &mut sink,
                tungstenite::Message::Ping(Vec::new().into()),
                &stop,
                &WriteProgress::new(),
            )
            .await
        );
        stop.stop();
        assert!(
            !send_frame(
                &mut sink,
                tungstenite::Message::Ping(Vec::new().into()),
                &stop,
                &WriteProgress::new(),
            )
            .await
        );
    }

    #[test]
    fn malformed_replies_never_log_the_payload_or_slice_unicode() {
        use std::io::Write;
        #[derive(Clone)]
        struct Log(Arc<Mutex<Vec<u8>>>);
        impl Write for Log {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Log(Arc::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        let waiters: Waiters = Arc::default();
        let (tx, mut rx) = oneshot::channel();
        waiters.lock().insert(1, tx);
        let (events, mut updates) = broadcast::channel(16);
        tracing::subscriber::with_default(subscriber, || {
            // Byte 200 is in the middle of the final character.
            handle_incoming(&format!("{}é", "x".repeat(199)), &waiters, &events);
            handle_incoming(
                r#"{"type":"unknown","private":"synthetic-private-sentinel"}"#,
                &waiters,
                &events,
            );
        });
        let text = String::from_utf8(log.0.lock().clone()).unwrap();
        assert_eq!(text.matches("unparseable server message").count(), 2);
        assert!(text.contains("bytes=201"), "{text}");
        assert!(text.contains("category=Syntax"), "{text}");
        assert!(text.contains("category=Data"), "{text}");
        assert!(!text.contains("synthetic-private-sentinel"));
        assert!(!text.contains("é"));
        assert!(!text.contains(&"x".repeat(20)));
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            updates.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn valid_replies_still_reach_their_waiter() {
        let waiters: Waiters = Arc::default();
        let (tx, mut rx) = oneshot::channel();
        waiters.lock().insert(42, tx);
        let (events, _) = broadcast::channel(16);
        let reply = ServerMessage::Response {
            id: 42,
            result: serde_json::json!({"ok": true}),
        };
        handle_incoming(&serde_json::to_string(&reply).unwrap(), &waiters, &events);
        assert_eq!(
            rx.try_recv().unwrap().unwrap(),
            serde_json::json!({"ok": true})
        );
        assert!(waiters.lock().is_empty());
    }

    /// Hold the outbound queue to deterministically reproduce a slow socket.
    fn queued_client() -> (Client, Arc<Outbox>) {
        let outbound = Arc::new(Outbox::default());
        let stop = Arc::new(tokio_util_lite::Stop::default());
        let rx = outbound.clone();
        let (_, status) = watch::channel(ConnectionStatus::new(ConnState::Connected, None, None));
        (
            Client {
                _lifetime: Arc::new(ClientLifetime {
                    outbound: outbound.clone(),
                    stop: stop.clone(),
                }),
                config: Arc::default(),
                outbound,
                waiters: Arc::default(),
                next_id: Arc::new(AtomicU64::new(1)),
                transport_epoch: Arc::new(AtomicU64::new(1)),
                instance_id: 1,
                events: broadcast::channel(16).0,
                status,
                stop,
                nudge: Arc::default(),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn epoch_scoped_ack_is_dropped_instead_of_migrating_to_reconnect() {
        let (client, rx) = queued_client();
        let epoch = client.connected_epoch().unwrap();
        client.post_in_epoch(epoch, Request::AckNotifications { ids: vec![903] });
        let queued = rx.recv().await.unwrap();
        assert!(
            queued
                .into_message_in_epoch(&client.waiters, epoch + 2)
                .is_none()
        );
        client.transport_epoch.store(epoch + 2, Ordering::SeqCst);
        client.post_in_epoch(epoch, Request::AckNotifications { ids: vec![903] });
        assert_eq!(rx.used().2, 0);
        client.post_in_epoch(epoch + 2, Request::AckNotifications { ids: vec![903] });
        assert!(
            rx.recv()
                .await
                .unwrap()
                .into_message_in_epoch(&client.waiters, epoch + 2)
                .is_some()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn timed_out_and_aborted_rpcs_are_not_sent_from_the_queue() {
        let (client, rx) = queued_client();
        let c = client.clone();
        let call = tokio::spawn(async move {
            c.call_timeout(Request::GetServerRoute, Duration::from_secs(1))
                .await
        });
        let queued = rx.recv().await.unwrap();
        assert_eq!(
            call.await.unwrap().unwrap_err().message,
            "request timed out"
        );
        assert!(queued.into_message(&client.waiters).is_none());
        assert!(client.waiters.lock().is_empty());

        let c = client.clone();
        let call = tokio::spawn(async move { c.call(Request::GetServerRoute).await });
        let queued = rx.recv().await.unwrap();
        call.abort();
        let _ = call.await;
        assert!(queued.into_message(&client.waiters).is_none());
        assert!(client.waiters.lock().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_discards_failed_rpcs_but_keeps_explicit_posts() {
        let (client, rx) = queued_client();
        let c = client.clone();
        let call = tokio::spawn(async move { c.call(Request::GetServerRoute).await });
        let queued = rx.recv().await.unwrap();
        // Exactly what the manager does on losing a connection, before it
        // moves the unsent queue into pending for the next connection.
        for (_, tx) in client.waiters.lock().drain() {
            let _ = tx.send(Err(ApiError::new(ErrorCode::Internal, "connection lost")));
        }
        assert_eq!(call.await.unwrap().unwrap_err().message, "connection lost");
        assert!(queued.into_message(&client.waiters).is_none());
        client.post(Request::GetServerRoute).unwrap();
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(
            rx.recv()
                .await
                .unwrap()
                .into_message(&client.waiters)
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_rpc_is_dropped_even_before_its_waiter_wakes() {
        let waiters: Waiters = Arc::default();
        let (tx, _rx) = oneshot::channel();
        waiters.lock().insert(1, tx);
        let queued = QueuedRequest {
            id: 1,
            request: Request::GetServerRoute,
            expires: Some(tokio::time::Instant::now()),
            epoch: None,
        };
        assert!(queued.into_message_in_epoch(&waiters, 0).is_none());
    }

    #[tokio::test]
    async fn proxy_headers_never_follow_a_redirect_to_another_host() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other = elsewhere.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(String::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = elsewhere.accept().await {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                seen2.lock().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = server.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://localhost:{other}/api/health\r\nContent-Length: 0\r\n\r\n"
                );
                let _ = s.write_all(reply.as_bytes()).await;
            }
        });
        let headers = BTreeMap::from([("CF-Access-Client-Secret".into(), "s3cret".into())]);
        assert!(probe(&[url], &headers).await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!seen.lock().contains("s3cret"), "{}", seen.lock());
    }

    #[tokio::test]
    async fn access_headers_go_only_to_https_addresses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A home address over plain http (on another network, anyone's).
        let lan = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", lan.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(String::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = lan.accept().await {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                seen2.lock().push_str(&String::from_utf8_lossy(&buf[..n]));
                let body = r#"{"name":"tessera"}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            }
        });
        let headers = BTreeMap::from([("CF-Access-Client-Secret".into(), "s3cret".into())]);
        assert_eq!(
            probe(std::slice::from_ref(&url), &headers).await.unwrap(),
            url
        );
        assert!(seen.lock().contains("GET /api/health"));
        assert!(!seen.lock().contains("s3cret"), "{}", seen.lock());
        let ws = ws_request(&url, "/api/secure", &headers).ok().unwrap();
        assert!(ws.headers().get("CF-Access-Client-Secret").is_none());
        // The tunnel's https address gets them.
        let ws = ws_request("https://t.example.com", "/api/secure", &headers)
            .ok()
            .unwrap();
        assert_eq!(ws.headers()["CF-Access-Client-Secret"], "s3cret");
    }

    #[tokio::test]
    async fn a_token_without_a_pinned_key_never_leaves_this_computer() {
        let config = ServerConfig {
            endpoints: vec!["http://192.0.2.1:7878".into()],
            token: "tsr_secret".into(),
            ..Default::default()
        };
        let failed = tokio::time::timeout(
            Duration::from_secs(2),
            open("http://192.0.2.1:7878", &config),
        )
        .await
        .expect("refused before connecting");
        let Err(f) = failed else { panic!("connected") };
        assert!(f.fatal && f.message.contains("pair this device again"));
    }

    #[tokio::test]
    async fn an_unreachable_server_is_explained_per_address() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A tunnel whose connector is down: Cloudflare answers 530.
        let tunnel = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_url = format!("http://{}", tunnel.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut s, _)) = tunnel.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 530 \r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        // A home address nothing answers on.
        let home = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let err = probe(&[home.clone(), tunnel_url.clone()], &BTreeMap::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("couldn't reach the server"), "{err}");
        assert!(
            err.contains(&format!(
                "{home}: no answer (not reachable from this network)"
            )),
            "{err}"
        );
        assert!(
            err.contains(&format!("{tunnel_url}: the tunnel isn't connected")),
            "{err}"
        );
    }

    fn check(
        ms: u64,
        ok: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<usize, String>> + Send>,
    > {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if ok {
                Ok(0)
            } else {
                Err(format!("failed after {ms}ms"))
            }
        })
    }

    #[tokio::test(start_paused = true)]
    async fn probing_takes_the_first_answer_without_waiting_for_slow_failures() {
        let grace = Duration::from_millis(400);
        // Preferred (LAN) address is unreachable and times out slowly; the
        // tunnel answers fast. We must not wait for the LAN timeout.
        let start = tokio::time::Instant::now();
        assert_eq!(
            race_preferred(vec![check(5000, false), check(150, true)], grace).await,
            Ok(1)
        );
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "{:?}",
            start.elapsed()
        );
        // The preferred address answering within the grace period still wins.
        assert_eq!(
            race_preferred(vec![check(300, true), check(100, true)], grace).await,
            Ok(0)
        );
        // As soon as the preferred one answers there is nothing to wait for.
        let start = tokio::time::Instant::now();
        assert_eq!(
            race_preferred(vec![check(50, true), check(3000, true)], grace).await,
            Ok(0)
        );
        assert!(start.elapsed() < Duration::from_millis(100));
        // A fast failure of the preferred address ends the wait immediately.
        let start = tokio::time::Instant::now();
        assert_eq!(
            race_preferred(
                vec![check(20, false), check(80, true), check(4000, true)],
                grace
            )
            .await,
            Ok(1)
        );
        assert!(start.elapsed() < Duration::from_millis(100));
        // Nothing reachable: every error is reported.
        let err = race_preferred(vec![check(10, false), check(20, false)], grace)
            .await
            .unwrap_err();
        assert_eq!(err.len(), 2);
    }

    #[test]
    fn only_this_machine_counts_as_loopback() {
        for e in [
            "http://127.0.0.1:7878",
            "http://localhost:7878",
            "http://[::1]:7878",
        ] {
            assert!(is_loopback_endpoint(e), "{e}");
        }
        for e in [
            "http://192.168.1.50:7878",
            "https://tessera.example.com",
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
        ] {
            assert!(!is_loopback_endpoint(e), "{e}");
        }
    }

    fn route(urls: &[&str], headers: &[(&str, &str)]) -> tessera_proto::ServerRoute {
        tessera_proto::ServerRoute {
            urls: urls.iter().map(|u| u.to_string()).collect(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A phone paired at home before remote access learns the tunnel's
    /// address and Access key; later changes replace what the server gave,
    /// and what the user added stays.
    #[test]
    fn a_saved_server_follows_the_route_its_server_gives() {
        let lan = "http://192.168.1.10:7878";
        let cf = [
            ("CF-Access-Client-Id", "id.access"),
            ("CF-Access-Client-Secret", "s1"),
        ];
        // Paired at home before remote access: only the home address.
        let (eps, hs, known) = merge_route(
            &strings(&[lan]),
            &BTreeMap::new(),
            None,
            &route(&[lan, "https://tessera.example.com/"], &cf),
        )
        .expect("changes");
        assert_eq!(eps, strings(&[lan, "https://tessera.example.com"]));
        assert_eq!(hs["CF-Access-Client-Secret"], "s1");
        assert_eq!(known.header_names.len(), 2);
        // The same answer again changes nothing.
        let same = route(&[lan, "https://tessera.example.com"], &cf);
        assert!(merge_route(&eps, &hs, Some(&known), &same).is_none());
        // The user adds an address and a header of their own on the device.
        let mut eps2 = eps.clone();
        eps2.push("https://vpn.example.net".into());
        let mut hs2 = hs.clone();
        hs2.insert("X-Mine".into(), "1".into());
        // The address changes and the key is renewed.
        let moved = route(
            &[lan, "https://t2.example.com"],
            &[
                ("CF-Access-Client-Id", "id.access"),
                ("CF-Access-Client-Secret", "s2"),
            ],
        );
        let (eps3, hs3, known3) = merge_route(&eps2, &hs2, Some(&known), &moved).unwrap();
        assert_eq!(
            eps3,
            strings(&[lan, "https://vpn.example.net", "https://t2.example.com"]),
            "the old tunnel address goes, the user's stays"
        );
        assert_eq!(hs3["CF-Access-Client-Secret"], "s2");
        assert_eq!(hs3["X-Mine"], "1");
        // Remote access removed: the tunnel's address and key go.
        let (eps4, hs4, _) = merge_route(&eps3, &hs3, Some(&known3), &route(&[lan], &[])).unwrap();
        assert_eq!(eps4, strings(&[lan, "https://vpn.example.net"]));
        assert_eq!(hs4.keys().collect::<Vec<_>>(), ["X-Mine"]);
    }

    #[test]
    fn a_route_never_strands_or_rewrites_a_local_server() {
        // The desktop app on the server's own computer: left alone.
        let local = strings(&["http://127.0.0.1:7878"]);
        let r = route(
            &["https://tessera.example.com"],
            &[("CF-Access-Client-Id", "x")],
        );
        assert!(merge_route(&local, &BTreeMap::new(), None, &r).is_none());
        // A server with no address to give: the saved ones stay.
        let saved = strings(&["http://10.0.0.5:7878"]);
        assert!(
            merge_route(&saved, &BTreeMap::new(), None, &route(&[], &[]))
                .is_some_and(|(e, _, _)| e == saved)
        );
        let known = KnownRoute {
            urls: saved.clone(),
            header_names: vec![],
            header_values: Some(BTreeMap::new()),
            managed_urls: Some(saved.clone()),
        };
        let (e, _, _) =
            merge_route(&saved, &BTreeMap::new(), Some(&known), &route(&[], &[])).unwrap();
        assert_eq!(e, saved, "never left without an address");
        // Junk from the server is ignored.
        let junk = route(
            &[
                "file:///etc/passwd",
                "javascript:alert(1)",
                "https://ok.example.com",
            ],
            &[
                ("Bad Header", "x"),
                ("X-Ok", "line\r\nInjected: 1"),
                ("Host", "elsewhere.example"),
                ("Sec-WebSocket-Key", "x"),
                ("X-Fine", "v"),
            ],
        );
        let (e, h, _) = merge_route(&saved, &BTreeMap::new(), None, &junk).unwrap();
        assert_eq!(
            e,
            strings(&["http://10.0.0.5:7878", "https://ok.example.com"])
        );
        assert_eq!(h.keys().collect::<Vec<_>>(), ["X-Fine"]);
    }

    #[test]
    fn route_follow_keeps_the_users_order_and_addresses() {
        let remote = "https://tessera.example.com";
        let lan = "http://192.168.1.10:7878";
        let saved = strings(&[remote, "https://vpn.example.net", lan]);
        let r = route(&[lan, remote], &[]);
        let (eps, hs, known) = merge_route(&saved, &BTreeMap::new(), None, &r).unwrap();
        assert_eq!(eps, saved);
        assert!(merge_route(&eps, &hs, Some(&known), &r).is_none());
        let (eps, _, _) = merge_route(&eps, &hs, Some(&known), &route(&[lan], &[])).unwrap();
        assert_eq!(
            eps, saved,
            "the user supplied the remote address, not route-follow"
        );
    }

    #[test]
    fn pasted_and_edited_headers_are_never_replaced_or_removed() {
        let saved = strings(&["https://tessera.example.com"]);
        let pasted = BTreeMap::from([("cf-access-client-secret".into(), "pasted".into())]);
        let r = route(&[&saved[0]], &[("CF-Access-Client-Secret", "server")]);
        let (eps, hs, known) = merge_route(&saved, &pasted, None, &r).unwrap();
        assert_eq!(hs, pasted);
        assert!(known.header_names.is_empty());
        assert!(merge_route(&eps, &hs, Some(&known), &r).is_none());
        let (_, hs, _) = merge_route(&eps, &hs, Some(&known), &route(&[], &[])).unwrap();
        assert_eq!(hs, pasted);

        let (eps, mut hs, known) = merge_route(&saved, &BTreeMap::new(), None, &r).unwrap();
        hs.insert("CF-Access-Client-Secret".into(), "edited".into());
        let (_, kept, _) = merge_route(&eps, &hs, Some(&known), &route(&[], &[])).unwrap();
        assert_eq!(kept, hs, "a user edit relinquishes server ownership");
    }

    #[test]
    fn qr_imported_routes_rotate_and_retire_without_losing_manual_overrides() {
        let link = tessera_proto::PairLink {
            urls: strings(&["http://192.168.1.10:7878", "https://old.example.com"]),
            headers: vec![("CF-Access-Client-Secret".into(), "old".into())],
            code: "12345678".into(),
            name: "Home".into(),
            fingerprint: None,
        };
        let known = KnownRoute::from_pair_link(&link);
        // The serialized snapshot survives an app restart, too.
        let known: KnownRoute =
            serde_json::from_str(&serde_json::to_string(&known).unwrap()).unwrap();
        let mut endpoints = link.urls.clone();
        endpoints.push("https://manual.example.com".into());
        let mut headers: BTreeMap<_, _> = link.headers.iter().cloned().collect();
        headers.insert("X-Manual".into(), "mine".into());
        let (eps, hs, known) = merge_route(
            &endpoints,
            &headers,
            Some(&known),
            &route(
                &["http://192.168.1.10:7878", "https://new.example.com"],
                &[("cf-access-client-secret", "new")],
            ),
        )
        .unwrap();
        assert!(!eps.contains(&"https://old.example.com".into()));
        assert!(eps.contains(&"https://manual.example.com".into()));
        assert_eq!(hs["cf-access-client-secret"], "new");
        assert_eq!(hs["X-Manual"], "mine");
        let mut edited = hs.clone();
        edited.insert("cf-access-client-secret".into(), "user edited".into());
        let (_, kept, _) = merge_route(&eps, &edited, Some(&known), &route(&[], &[])).unwrap();
        assert_eq!(kept, edited);
        let (eps, hs, _) = merge_route(&eps, &hs, Some(&known), &route(&[], &[])).unwrap();
        assert_eq!(eps, strings(&["https://manual.example.com"]));
        assert_eq!(hs, BTreeMap::from([("X-Manual".into(), "mine".into())]));
    }

    /// Legacy QR imports and typed settings had identical on-disk shape.
    /// Never guess and overwrite a manual secret; re-pair records ownership.
    #[test]
    fn legacy_unowned_routes_are_preserved_until_explicit_qr_repair() {
        let saved = strings(&["http://192.168.1.10:7878", "https://old.example.com"]);
        let headers = BTreeMap::from([("CF-Access-Client-Secret".into(), "old".into())]);
        let r = route(
            &["http://192.168.1.10:7878"],
            &[("CF-Access-Client-Secret", "new")],
        );
        let (_, kept, _) = merge_route(&saved, &headers, None, &r).unwrap();
        assert_eq!(kept, headers);
        let link = tessera_proto::PairLink {
            urls: saved.clone(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            code: "12345678".into(),
            name: "Home".into(),
            fingerprint: None,
        };
        let (_, refreshed, _) = merge_route(
            &saved,
            &headers,
            Some(&KnownRoute::from_pair_link(&link)),
            &r,
        )
        .unwrap();
        assert_eq!(refreshed["CF-Access-Client-Secret"], "new");
    }

    #[test]
    fn legacy_header_ownership_is_not_guessed() {
        let saved = strings(&["https://tessera.example.com"]);
        let pasted = BTreeMap::from([("CF-Access-Client-Secret".into(), "pasted".into())]);
        let old: KnownRoute = serde_json::from_value(serde_json::json!({
            "urls": saved, "headerNames": ["CF-Access-Client-Secret"]
        }))
        .unwrap();
        let (eps, kept, _) = merge_route(&saved, &pasted, Some(&old), &route(&[], &[])).unwrap();
        assert_eq!(eps, saved, "legacy addresses might have been user supplied");
        assert_eq!(kept, pasted, "old apps didn't record whose value this was");
    }

    #[test]
    fn proxy_headers_never_go_to_tls_lan_addresses() {
        let headers = BTreeMap::from([("CF-Access-Client-Secret".into(), "pasted".into())]);
        for endpoint in [
            "https://192.168.1.10:7878",
            "https://10.0.0.1",
            "https://172.16.0.1",
            "https://127.0.0.1",
            "https://[::1]",
            "https://[fd12::1]",
            "https://[fe80::1]",
            "https://[::ffff:192.168.1.10]",
            "https://my-server.local",
            "https://my-server.lan",
            "https://my-server.home.arpa",
            "https://LOCALHOST.",
            "https://100.100.100.100",
            "https://my-server",
        ] {
            let req = ws_request(endpoint, "/api/secure", &headers).ok().unwrap();
            assert!(
                !req.headers().contains_key("CF-Access-Client-Secret"),
                "{endpoint}"
            );
            let req = apply_headers(http_client().get(endpoint), endpoint, &headers)
                .build()
                .unwrap();
            assert!(
                !req.headers().contains_key("CF-Access-Client-Secret"),
                "{endpoint}"
            );
        }
        for endpoint in ["https://tessera.example.com", "https://203.0.113.2"] {
            let req = ws_request(endpoint, "/api/secure", &headers).ok().unwrap();
            assert_eq!(req.headers()["CF-Access-Client-Secret"], "pasted");
            let req = apply_headers(http_client().get(endpoint), endpoint, &headers)
                .build()
                .unwrap();
            assert_eq!(req.headers()["CF-Access-Client-Secret"], "pasted");
        }
    }

    #[test]
    fn last_success_is_a_preference_not_a_saved_order_change() {
        let saved = strings(&[
            "http://192.168.1.10:7878",
            "https://tessera.example.com",
            "https://vpn.example.net",
        ]);
        let mut attempts = saved.clone();
        prefer_last(&mut attempts, Some("https://tessera.example.com/"));
        assert_eq!(
            attempts,
            strings(&[
                "https://tessera.example.com",
                "http://192.168.1.10:7878",
                "https://vpn.example.net"
            ])
        );
        prefer_last(&mut attempts, Some("https://retired.example.com"));
        assert_eq!(attempts[0], "https://tessera.example.com");
        assert_eq!(saved[0], "http://192.168.1.10:7878");
    }

    #[tokio::test(start_paused = true)]
    async fn last_success_avoids_even_the_lan_grace_period() {
        let start = tokio::time::Instant::now();
        assert_eq!(
            race_preferred(
                vec![check(100, true), check(5000, false)],
                Duration::from_millis(400)
            )
            .await,
            Ok(0)
        );
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        // Network changed: the saved winner stalls, but the other one still wins promptly.
        let start = tokio::time::Instant::now();
        assert_eq!(
            race_preferred(
                vec![check(5000, false), check(100, true)],
                Duration::from_millis(400)
            )
            .await,
            Ok(1)
        );
        assert_eq!(start.elapsed(), Duration::from_millis(500));
    }

    #[test]
    fn normalizes_endpoints() {
        assert_eq!(normalize("192.168.1.5:7878/"), "http://192.168.1.5:7878");
        assert_eq!(normalize("https://t.example.com"), "https://t.example.com");
    }

    #[test]
    fn config_serde_is_camel_case() {
        let c = ServerConfig {
            endpoints: vec!["a".into()],
            headers: BTreeMap::from([("CF-Access-Client-Id".into(), "x".into())]),
            token: "t".into(),
            server_key: "k".into(),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert!(
            v.get("endpoints").is_some()
                && v.get("headers").is_some()
                && v.get("serverKey").is_some()
        );
        // Configs saved before encryption still load.
        let old: ServerConfig = serde_json::from_str(r#"{"endpoints":["a"],"token":"t"}"#).unwrap();
        assert!(old.server_key.is_empty());
    }
}
