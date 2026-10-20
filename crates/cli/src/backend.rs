//! Where the CLI's engine lives: in-process, a local daemon, or a remote
//! server. All three look the same to the rest of the CLI.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tessera_client::{Client, ClientEvent, ConnState, ConnectionStatus, ServerConfig};
use tessera_core::config::Paths;
use tessera_core::{Engine, EngineOptions};
use tessera_proto::{ApiError, Event, Request};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // short-lived; boxing every event isn't worth it
pub enum BackendEvent {
    Event(Event),
    Status(ConnectionStatus),
    /// Reconnected after a drop: resynchronise.
    Reconnected,
}

#[derive(Clone)]
pub enum Backend {
    Local(Engine),
    Remote {
        client: Client,
        label: String,
        /// The server runs on this machine (found through its lock file).
        local: bool,
    },
}

/// Saved remote server for the CLI (`tessera connect`), or the machine's
/// login (`node-server.json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientFile {
    pub name: String,
    pub server: ServerConfig,
    /// The token is a machine login: it can only serve as this computer's
    /// machine (`tessera node`), never act as one of the user's devices.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub machine: bool,
    /// For a machine login traded for a device's own login (the desktop
    /// app's): that login's SHA-256, so turning the computer on again with
    /// it keeps this one instead of replacing the login a running node uses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub traded_from: String,
    /// What the server last said about how to reach it (`get_server_route`):
    /// which of the addresses and headers above came from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<tessera_client::KnownRoute>,
}

/// Keep a saved login (`client.json`, a machine's `node-server.json`) up
/// with how its server says devices reach it: now, after every reconnect,
/// and whenever the server says it changed (remote access set up, changed
/// or removed). `load` and `save` read and write that file; a file that
/// holds another login by then is left alone.
pub fn follow_route(
    client: &Client,
    load: impl Fn() -> Option<ClientFile> + Send + Sync + 'static,
    save: impl Fn(&ClientFile) -> Result<()> + Send + Sync + 'static,
) {
    let token = match load() {
        Some(f) => f.server.token,
        None => return,
    };
    let client = client.clone();
    let mut events = client.subscribe();
    tokio::spawn(async move {
        let sync = async |client: &Client| {
            let Some(route) = client.server_route().await else {
                return;
            };
            let Some(mut f) = load().filter(|f| f.server.token == token) else {
                return;
            };
            if let Some((endpoints, headers, known)) = tessera_client::merge_route(
                &f.server.endpoints,
                &f.server.headers,
                f.route.as_ref(),
                &route,
            ) {
                f.server.endpoints = endpoints.clone();
                f.server.headers = headers.clone();
                f.route = Some(known);
                if save(&f).is_ok() {
                    client.set_route(endpoints, headers);
                }
            }
        };
        if client.status().state == ConnState::Connected {
            sync(&client).await;
        }
        loop {
            match events.recv().await {
                Ok(ClientEvent::Connected | ClientEvent::Event(Event::ServerRouteChanged)) => {
                    sync(&client).await
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

pub fn client_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join("client.json")
}

pub fn load_client_file(paths: &Paths) -> Option<ClientFile> {
    let text = std::fs::read_to_string(client_file(paths)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_client_file(paths: &Paths, f: &ClientFile) -> Result<()> {
    tessera_core::config::write_atomic(
        &client_file(paths),
        serde_json::to_string_pretty(f)?.as_bytes(),
        true,
    )
}

/// Written by `tessera serve` so local CLIs attach to it instead of opening
/// the database in a second engine.
#[derive(Debug, Serialize, Deserialize)]
pub struct ServerLock {
    pub pid: u32,
    pub url: String,
    /// The server's public key: local clients connect end-to-end encrypted too.
    #[serde(default)]
    pub key: String,
}

pub fn lock_file(paths: &Paths) -> PathBuf {
    paths.data_dir.join("server.json")
}

pub async fn local_daemon(paths: &Paths) -> Option<(String, String)> {
    let lock: ServerLock =
        serde_json::from_str(&std::fs::read_to_string(lock_file(paths)).ok()?).ok()?;
    let url = tessera_client::probe(std::slice::from_ref(&lock.url), &Default::default())
        .await
        .ok()?;
    Some((url, lock.key))
}

/// Token for the local CLI device, created directly in the shared database.
fn local_token(paths: &Paths) -> Result<String> {
    let file = paths.data_dir.join("cli-token");
    let store = tessera_core::store::Store::open(&paths.db_file())?;
    if let Ok(t) = std::fs::read_to_string(&file) {
        let t = t.trim().to_string();
        if store.authenticate(&t)?.is_some() {
            return Ok(t);
        }
    }
    let (_, token) = store.add_device("Terminal (this machine)")?;
    tessera_core::config::write_atomic(&file, token.as_bytes(), true)?;
    Ok(token)
}

/// The in-process engine's view of its browser (frames for the web views
/// this terminal shows), made by `subscribe`.
static LOCAL_VIEWER: std::sync::Mutex<Option<std::sync::Arc<tessera_core::browser::Viewer>>> =
    std::sync::Mutex::new(None);

/// Tell the server this login is the terminal, and which computer it runs
/// on (Settings → Devices shows it with that computer's app and machine).
/// Nobody waits for the answer; an older server just refuses it.
fn say_hello(client: &Client) -> Result<()> {
    client
        .post(Request::DeviceHello {
            client: tessera_proto::DeviceClient::Terminal,
            computer: tessera_client::computer_key(),
            host: tessera_client::host_name(),
            model: None,
        })
        .map_err(|e| anyhow::anyhow!(e.message))
}

pub struct BackendChoice {
    pub force_local: bool,
}

impl Backend {
    pub async fn open(paths: &Paths, choice: BackendChoice) -> Result<Self> {
        if !choice.force_local {
            if let Some(f) = load_client_file(paths) {
                let client = Client::connect(f.server.clone());
                client
                    .wait_connected(Duration::from_secs(8))
                    .await
                    .with_context(|| {
                        format!(
                            "connecting to {}",
                            if f.name.is_empty() {
                                "the saved server"
                            } else {
                                &f.name
                            }
                        )
                    })?;
                say_hello(&client)?;
                let p = paths.clone();
                let q = paths.clone();
                follow_route(
                    &client,
                    move || load_client_file(&p).filter(|f| !f.machine),
                    move |f| save_client_file(&q, f),
                );
                let label = client.status().endpoint.unwrap_or_default();
                return Ok(Backend::Remote {
                    client,
                    label,
                    local: false,
                });
            }
            if let Some((url, server_key)) = local_daemon(paths).await {
                let token = local_token(paths)?;
                let client = Client::connect(ServerConfig {
                    endpoints: vec![url.clone()],
                    headers: Default::default(),
                    token,
                    server_key,
                });
                if client.wait_connected(Duration::from_secs(5)).await.is_ok() {
                    say_hello(&client)?;
                    return Ok(Backend::Remote {
                        client,
                        label: format!("local server {url}"),
                        local: true,
                    });
                }
            }
        }
        let engine = Engine::open(paths.clone(), EngineOptions { scheduler: false }).await?;
        Ok(Backend::Local(engine))
    }

    pub fn describe(&self) -> String {
        match self {
            Backend::Local(_) => "local".into(),
            Backend::Remote { label, client, .. } => {
                if client.status().encrypted {
                    format!("{label} · end-to-end encrypted")
                } else {
                    label.clone()
                }
            }
        }
    }

    pub async fn call(&self, req: Request) -> Result<Value, ApiError> {
        match self {
            Backend::Local(e) => {
                let viewer = LOCAL_VIEWER.lock().ok().and_then(|v| v.clone());
                e.handle_as(req, None, viewer.as_deref()).await
            }
            Backend::Remote { client, .. } => client.call(req).await,
        }
    }

    pub async fn call_as<T: serde::de::DeserializeOwned>(
        &self,
        req: Request,
    ) -> Result<T, ApiError> {
        let v = self.call(req).await?;
        serde_json::from_value(v).map_err(|e| {
            ApiError::new(
                tessera_proto::ErrorCode::Internal,
                format!("unexpected response: {e}"),
            )
        })
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<BackendEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        match self {
            Backend::Local(engine) => {
                // Web view frames come on a channel of their own.
                let (viewer, mut frames) = engine.browser_viewer();
                if let Ok(mut v) = LOCAL_VIEWER.lock() {
                    *v = Some(std::sync::Arc::new(viewer));
                }
                let ftx = tx.clone();
                tokio::spawn(async move {
                    while let Some(ev) = frames.recv().await {
                        if ftx.send(BackendEvent::Event(ev)).is_err() {
                            break;
                        }
                    }
                });
                let mut sub = engine.subscribe();
                tokio::spawn(async move {
                    loop {
                        match sub.recv().await {
                            Ok(ev) => {
                                if tx.send(BackendEvent::Event(ev)).is_err() {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if tx.send(BackendEvent::Event(Event::Resync)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
            Backend::Remote { client, .. } => {
                let mut sub = client.subscribe();
                tokio::spawn(async move {
                    loop {
                        let ev = match sub.recv().await {
                            Ok(ClientEvent::Event(e)) => BackendEvent::Event(e),
                            Ok(ClientEvent::Status(s)) => BackendEvent::Status(s),
                            Ok(ClientEvent::Connected) => BackendEvent::Reconnected,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                BackendEvent::Event(Event::Resync)
                            }
                            Err(_) => break,
                        };
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        rx
    }

    /// Drop the connection (remote) — the engine of a local backend stops
    /// with its last clone.
    pub fn close(&self) {
        if let Backend::Remote { client, .. } = self {
            client.close();
        }
    }

    pub fn connected(&self) -> bool {
        match self {
            Backend::Local(_) => true,
            Backend::Remote { client, .. } => client.status().state == ConnState::Connected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The terminal paired at home before remote access: when the server
    /// gets a remote address and Access key, `client.json` learns them by
    /// itself (the address the user added stays), and forgets them again
    /// when remote access goes.
    #[tokio::test]
    async fn the_saved_login_follows_the_servers_route() {
        let dir = tempfile::tempdir().unwrap();
        let server = Paths::at(dir.path().join("server"));
        let engine = Engine::open(server, EngineOptions { scheduler: false })
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = tessera_server::router(engine.clone(), None).unwrap();
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
        let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
        let (_, grant) = tessera_client::pair(
            std::slice::from_ref(&url),
            &BTreeMap::new(),
            &code,
            "Terminal",
        )
        .await
        .unwrap();
        let here = Paths::at(dir.path().join("terminal"));
        // Its home address, plus one the user typed in themselves.
        let mine = "http://192.0.2.1:7878".to_string();
        let file = ClientFile {
            name: "Home".into(),
            server: ServerConfig {
                endpoints: vec![url.clone(), mine.clone()],
                headers: BTreeMap::new(),
                token: grant.token,
                server_key: grant.server_key,
            },
            ..Default::default()
        };
        save_client_file(&here, &file).unwrap();
        let client = Client::connect(file.server.clone());
        client
            .wait_connected(Duration::from_secs(10))
            .await
            .unwrap();
        let (p, q) = (here.clone(), here.clone());
        follow_route(
            &client,
            move || load_client_file(&p),
            move |f| save_client_file(&q, f),
        );
        let saved = |want: fn(&ClientFile) -> bool| {
            let here = here.clone();
            async move {
                for _ in 0..100 {
                    if let Some(f) = load_client_file(&here).filter(want) {
                        return Some(f);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                None
            }
        };
        let remote =
            |urls: Vec<String>, headers: BTreeMap<String, String>| Request::SetRemoteAccess {
                urls,
                headers: Some(headers),
                add_headers: None,
                remove_headers: vec![],
            };
        engine
            .handle(
                remote(
                    vec!["https://tessera.example.com".into()],
                    BTreeMap::from([("CF-Access-Client-Secret".into(), "s3cret".into())]),
                ),
                None,
            )
            .await
            .unwrap();
        let f = saved(|f| f.server.headers.contains_key("CF-Access-Client-Secret"))
            .await
            .expect("client.json learned the route");
        assert_eq!(
            f.server.endpoints,
            [
                url.clone(),
                mine.clone(),
                "https://tessera.example.com".to_string()
            ],
            "route-follow keeps the terminal user's saved address order"
        );
        assert_eq!(f.server.headers["CF-Access-Client-Secret"], "s3cret");
        assert!(f.route.is_some());

        engine
            .handle(remote(vec![], BTreeMap::new()), None)
            .await
            .unwrap();
        let f = saved(|f| f.server.headers.is_empty())
            .await
            .expect("client.json forgot it");
        assert_eq!(f.server.endpoints, [url, mine]);
        client.close();
    }
}
