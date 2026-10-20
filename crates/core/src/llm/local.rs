//! Finding AI servers on this machine and on the home network: llama.cpp,
//! Ollama, LM Studio and vLLM all answer `GET /v1/models` with the models
//! they have. Only that list is asked for, which never loads a model.
//!
//! The home network is the /24 around this machine's own private address,
//! on the four usual ports; a host that doesn't answer within a moment is
//! passed over, so a look takes a few seconds at most.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;

/// A server that answered with at least one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// `http://host:port/v1`
    pub base_url: String,
    /// What it is, from its answer (or the port): "Ollama", "LM Studio"…
    pub server: &'static str,
    pub models: Vec<String>,
}

/// The ports looked at on other computers: Ollama, LM Studio (and the
/// llama.cpp router), llama.cpp's own server, vLLM.
pub const LAN_PORTS: [u16; 4] = [11434, 1234, 8080, 8000];

/// How long a host on the network gets to accept a connection.
const CONNECT: Duration = Duration::from_millis(400);
/// Connections tried at once.
const AT_ONCE: usize = 128;

/// The other addresses of this machine's /24, when its address is a private
/// (home or office) one. Empty otherwise: nothing public is ever scanned.
pub fn lan_hosts() -> Vec<Ipv4Addr> {
    match crate::util::lan_ip() {
        Some(IpAddr::V4(me)) => neighbours(me),
        _ => Vec::new(),
    }
}

fn neighbours(me: Ipv4Addr) -> Vec<Ipv4Addr> {
    if !me.is_private() {
        return Vec::new();
    }
    let [a, b, c, _] = me.octets();
    (1..=254)
        .map(|d| Ipv4Addr::new(a, b, c, d))
        .filter(|ip| *ip != me)
        .collect()
}

/// Every server among `hosts` × `ports` that lists models. Hosts are tried
/// [`AT_ONCE`] at a time; one that doesn't accept within `connect` is skipped.
pub async fn find_on(
    http: &reqwest::Client,
    hosts: &[IpAddr],
    ports: &[u16],
    connect: Duration,
) -> Vec<Found> {
    let targets: Vec<SocketAddr> = hosts
        .iter()
        .flat_map(|h| ports.iter().map(move |p| SocketAddr::new(*h, *p)))
        .collect();
    let open: Vec<SocketAddr> = futures::stream::iter(targets)
        .map(|addr| async move {
            matches!(
                tokio::time::timeout(connect, tokio::net::TcpStream::connect(addr)).await,
                Ok(Ok(_))
            )
            .then_some(addr)
        })
        .buffer_unordered(AT_ONCE)
        .filter_map(|a| async move { a })
        .collect()
        .await;
    let mut found: Vec<Found> = futures::future::join_all(open.into_iter().map(|addr| {
        let base = match addr.ip() {
            IpAddr::V6(ip) => format!("http://[{ip}]:{}/v1", addr.port()),
            IpAddr::V4(ip) => format!("http://{ip}:{}/v1", addr.port()),
        };
        async move { list(http, &base, addr.port()).await }
    }))
    .await
    .into_iter()
    .flatten()
    .collect();
    found.sort_by(|a, b| a.base_url.cmp(&b.base_url));
    found
}

/// Servers on the home network (see [`lan_hosts`]).
pub async fn find_on_lan(http: &reqwest::Client) -> Vec<Found> {
    let hosts: Vec<IpAddr> = lan_hosts().into_iter().map(IpAddr::V4).collect();
    if hosts.is_empty() {
        return Vec::new();
    }
    find_on(http, &hosts, &LAN_PORTS, CONNECT).await
}

/// `GET {base}/models`: the server and its models, if it is a model server.
async fn list(http: &reqwest::Client, base: &str, port: u16) -> Option<Found> {
    let resp = http
        .get(format!("{base}/models"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    super::check_redirect(&resp).ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = serde_json::from_str(&resp.text().await.ok()?).ok()?;
    let (server, models) = read_list(&v, port)?;
    Some(Found {
        base_url: base.to_string(),
        server,
        models,
    })
}

/// What a `/models` answer says: the kind of server and its model ids.
/// `None` for anything that isn't an OpenAI-style list with a model in it.
pub fn read_list(v: &Value, port: u16) -> Option<(&'static str, Vec<String>)> {
    let list = v.get("data").and_then(Value::as_array)?;
    let models: Vec<String> = list
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    if models.is_empty() {
        return None;
    }
    let owner = list
        .first()
        .and_then(|m| m.get("owned_by"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let server = match owner {
        "llamacpp" => "llama.cpp",
        "library" => "Ollama",
        "organization_owner" => "LM Studio",
        "vllm" => "vLLM",
        _ => match port {
            11434 => "Ollama",
            1234 => "LM Studio",
            8000 => "vLLM",
            8080 => "llama.cpp",
            _ => "AI server",
        },
    };
    Some((server, models))
}

/// A provider id for a server elsewhere on the network: its kind and the
/// last part of its address ("ollama-42"), or the port when that isn't
/// enough ("llama-cpp-42-8080").
pub fn lan_provider_id(f: &Found, taken: impl Fn(&str) -> bool) -> String {
    let kind: String = f
        .server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let rest = f
        .base_url
        .trim_start_matches("http://")
        .trim_end_matches("/v1");
    let (host, port) = rest.rsplit_once(':').unwrap_or((rest, ""));
    let last = host.rsplit('.').next().unwrap_or(host);
    let id = format!("{kind}-{last}");
    if !taken(&id) {
        return id;
    }
    format!("{kind}-{last}-{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn only_private_neighbours() {
        let n = neighbours(Ipv4Addr::new(192, 168, 1, 193));
        assert_eq!(n.len(), 253);
        assert!(!n.contains(&Ipv4Addr::new(192, 168, 1, 193)));
        assert!(n.contains(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(n.contains(&Ipv4Addr::new(192, 168, 1, 254)));
        assert_eq!(neighbours(Ipv4Addr::new(10, 0, 0, 5)).len(), 253);
        // Public addresses and Tailscale's (CGNAT) range are never scanned.
        assert!(neighbours(Ipv4Addr::new(203, 0, 113, 7)).is_empty());
        assert!(neighbours(Ipv4Addr::new(100, 83, 34, 68)).is_empty());
    }

    #[test]
    fn answers_are_read() {
        let v: Value = serde_json::json!({"object": "list", "data": [
            {"id": "qwen3:8b", "owned_by": "library"}, {"id": "llama3.2"}]});
        assert_eq!(
            read_list(&v, 11434),
            Some(("Ollama", vec!["qwen3:8b".into(), "llama3.2".into()]))
        );
        let v = serde_json::json!({"data": [{"id": "m", "owned_by": "llamacpp"}]});
        assert_eq!(read_list(&v, 1234).unwrap().0, "llama.cpp");
        let v = serde_json::json!({"data": [{"id": "m", "owned_by": "organization_owner"}]});
        assert_eq!(read_list(&v, 1234).unwrap().0, "LM Studio");
        let v = serde_json::json!({"data": [{"id": "m"}]});
        assert_eq!(read_list(&v, 8000).unwrap().0, "vLLM");
        // Something else on a common port: an empty list, or no list at all.
        assert!(read_list(&serde_json::json!({"data": []}), 8080).is_none());
        assert!(read_list(&serde_json::json!({"status": "ok"}), 8080).is_none());
    }

    #[test]
    fn ids_for_servers_elsewhere() {
        let f = Found {
            base_url: "http://192.168.1.42:11434/v1".into(),
            server: "Ollama",
            models: vec!["m".into()],
        };
        assert_eq!(lan_provider_id(&f, |_| false), "ollama-42");
        assert_eq!(
            lan_provider_id(&f, |id| id == "ollama-42"),
            "ollama-42-11434"
        );
        let f = Found {
            base_url: "http://10.0.0.7:8080/v1".into(),
            server: "llama.cpp",
            models: vec!["m".into()],
        };
        assert_eq!(lan_provider_id(&f, |_| false), "llama-cpp-7");
        let f = Found {
            base_url: "http://10.0.0.7:1234/v1".into(),
            server: "LM Studio",
            models: vec!["m".into()],
        };
        assert_eq!(lan_provider_id(&f, |_| false), "lm-studio-7");
    }

    /// A server that answers every request with `body`.
    async fn serve(body: &'static str) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(reply.as_bytes()).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn finds_model_servers_and_passes_over_the_rest() {
        let ollama = serve(r#"{"data":[{"id":"qwen3:8b","owned_by":"library"}]}"#).await;
        let other = serve(r#"{"hello":"world"}"#).await;
        // A port nothing listens on.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let http = reqwest::Client::new();
        let found = find_on(
            &http,
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
            &[ollama, other, closed],
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(
            found,
            vec![Found {
                base_url: format!("http://127.0.0.1:{ollama}/v1"),
                server: "Ollama",
                models: vec!["qwen3:8b".into()],
            }]
        );
    }
}
