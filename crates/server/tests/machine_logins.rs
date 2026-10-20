//! Machine logins: what `tessera node` keeps on a computer can only serve as
//! that machine. Whoever reads it there can't read chats, pair devices or
//! run the server's agent with it.

use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ClientEvent, ServerConfig};
use tessera_core::config::Paths;
use tessera_proto::*;

async fn start() -> (tessera_core::Engine, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "default_model = \"x/m\"\n[[provider]]\nid = \"x\"\nkind = \"openai_chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\n[[provider.models]]\nid = \"m\"\n",
    )
    .unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = tessera_server::router(engine.clone(), None).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    (engine, format!("http://{addr}"), dir)
}

fn hello(name: &str) -> Request {
    Request::NodeHello {
        name: name.into(),
        os: "linux".into(),
        version: "test".into(),
        scope: MachineScope::default(),
        generation: 1,
        sandbox: "on".into(),
        sandbox_ok: true,
        home: None,
        computer: None,
        host: None,
        features: None,
        guard_key: None,
    }
}

/// Everything a device may do that a machine must not.
fn device_requests() -> Vec<Request> {
    vec![
        Request::ListSessions { archived: false },
        Request::CreateSession {
            cwd: None,
            model: None,
            title: Some("from the machine".into()),
            bot_id: None,
            private: false,
            lean: false,
        },
        Request::CreatePairing,
        Request::ListDevices,
        Request::MachineLogin,
        Request::ListSecrets,
        Request::ListMachines,
        Request::RevokeDevice { id: "x".into() },
        Request::RenameDevice {
            id: "x".into(),
            name: "Renamed by a machine".into(),
        },
        Request::DeviceHello {
            client: DeviceClient::App,
            computer: None,
            host: None,
            model: None,
        },
        Request::ListDir { path: "/".into() },
        // Refused before it reaches the engine: nothing is made.
        Request::CreateDir {
            parent: "/tmp".into(),
            name: "made-by-a-machine-login".into(),
        },
    ]
}

async fn connected(config: ServerConfig) -> Client {
    let c = Client::connect(config);
    c.wait_connected(Duration::from_secs(10)).await.unwrap();
    c
}

async fn assert_only_a_machine(client: &Client, what: &str) {
    for req in device_requests() {
        let name = serde_json::to_value(&req).unwrap()["method"].clone();
        let err = client.call(req).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized, "{what}: {name}");
        assert!(err.message.contains("machine"), "{what}: {}", err.message);
    }
}

#[tokio::test]
async fn a_computer_paired_as_a_machine_can_only_serve_as_it() {
    let (engine, url, _dir) = start().await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair_as(
        std::slice::from_ref(&url),
        &BTreeMap::new(),
        &code,
        "Build box",
        true,
    )
    .await
    .unwrap();
    assert!(grant.machine, "the server made it a machine login");
    let machine = connected(ServerConfig {
        endpoints: vec![url.clone()],
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: grant.server_key.clone(),
    })
    .await;
    let mut hears = machine.subscribe();

    // Hello tells it who the server is, nothing about the owner's setup.
    let info: ServerInfo = machine
        .call_as(Request::Hello {
            client: "test".into(),
            version: "0".into(),
        })
        .await
        .unwrap();
    assert!(info.home.is_empty() && info.default_model.is_none());
    // Refused even before it says it is a machine…
    assert_only_a_machine(&machine, "before node_hello").await;
    let welcome: NodeWelcome = machine.call_as(hello("Build box")).await.unwrap();
    assert_eq!(welcome.machine_id, grant.device_id);
    // …and after.
    assert_only_a_machine(&machine, "after node_hello").await;
    let list: Vec<MachineInfo> =
        serde_json::from_value(engine.handle(Request::ListMachines, None).await.unwrap()).unwrap();
    assert!(list.iter().any(|m| m.id == grant.device_id && m.online));

    // The chats' events never reach it.
    let _: SessionInfo = serde_json::from_value(
        engine
            .handle(
                Request::CreateSession {
                    cwd: None,
                    model: None,
                    title: Some("private plans".into()),
                    bot_id: None,
                    private: false,
                    lean: false,
                },
                None,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let heard = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(ClientEvent::Event(e)) = hears.recv().await {
                return e;
            }
        }
    })
    .await;
    assert!(heard.is_err(), "a machine heard {heard:?}");
    // The plain socket (the browser's, on this computer) is held to it too.
    let plain = connected(ServerConfig {
        endpoints: vec![url],
        token: grant.token,
        ..Default::default()
    })
    .await;
    assert_only_a_machine(&plain, "plain socket").await;
    machine.close();
    plain.close();
}

#[tokio::test]
async fn a_device_gives_its_computers_machine_a_login_of_its_own() {
    let (engine, url, _dir) = start().await;
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    // The desktop app: an ordinary device (servers before machine logins
    // and older apps pair this way).
    let (_, app) = tessera_client::pair(
        std::slice::from_ref(&url),
        &BTreeMap::new(),
        &code,
        "Desktop",
    )
    .await
    .unwrap();
    assert!(!app.machine);
    let app_login = ServerConfig {
        endpoints: vec![url.clone()],
        headers: BTreeMap::new(),
        token: app.token.clone(),
        server_key: app.server_key.clone(),
    };
    // Turning this computer on as a machine trades the app's login for one
    // that can only serve as the machine, under the app's device id (so the
    // machine keeps its name, bots' access and allowed passwords).
    let machine_login = tessera_client::machine_login(&app_login).await.unwrap();
    assert_ne!(machine_login.token, app_login.token);
    let machine = connected(machine_login.clone()).await;
    let welcome: NodeWelcome = machine.call_as(hello("Desktop")).await.unwrap();
    assert_eq!(welcome.machine_id, app.device_id);
    assert_only_a_machine(&machine, "the app's machine").await;

    // The app keeps working, alongside its machine.
    let desktop = connected(app_login.clone()).await;
    let list: Vec<MachineInfo> = desktop.call_as(Request::ListMachines).await.unwrap();
    assert!(list.iter().any(|m| m.id == app.device_id && m.online));
    let devices: Vec<DeviceInfo> = desktop.call_as(Request::ListDevices).await.unwrap();
    assert_eq!(devices.len(), 1, "still one entry under Devices");

    // Asking again replaces the machine's login: the old one stops working.
    let newer = tessera_client::machine_login(&app_login).await.unwrap();
    machine.close();
    let stale = Client::connect(machine_login);
    assert!(stale.wait_connected(Duration::from_secs(3)).await.is_err());
    stale.close();
    let again = connected(newer.clone()).await;
    let _: NodeWelcome = again.call_as(hello("Desktop")).await.unwrap();

    // Removing the device (from another device) ends its machine's login.
    engine
        .handle(
            Request::RevokeDevice {
                id: app.device_id.clone(),
            },
            None,
        )
        .await
        .unwrap();
    let gone = Client::connect(newer);
    assert!(gone.wait_connected(Duration::from_secs(3)).await.is_err());
    gone.close();
    again.close();
    desktop.close();
}

#[tokio::test]
async fn a_machine_on_a_devices_own_login_serves_only_as_the_machine_on_that_connection() {
    // Machines turned on before machine logins use the desktop app's (or
    // the terminal's) own login until their `tessera node` is updated: they
    // keep working, and the connection that said node_hello is held to it.
    let (engine, url, _dir) = start().await;
    let (device, token) = engine.store().add_device("Desktop").unwrap();
    let key = tessera_secure::b64(
        tessera_server::load_identity(engine.paths())
            .unwrap()
            .public(),
    );
    let login = ServerConfig {
        endpoints: vec![url],
        headers: BTreeMap::new(),
        token,
        server_key: key,
    };
    let node = connected(login.clone()).await;
    let welcome: NodeWelcome = node.call_as(hello("Desktop")).await.unwrap();
    assert_eq!(welcome.machine_id, device);
    assert_only_a_machine(&node, "old node").await;
    // The app on the same login, on its own connection, is a device as before.
    let app = connected(login).await;
    let _: Vec<SessionInfo> = app
        .call_as(Request::ListSessions { archived: false })
        .await
        .unwrap();
    let _: Vec<DeviceInfo> = app.call_as(Request::ListDevices).await.unwrap();
    node.close();
    app.close();
}
