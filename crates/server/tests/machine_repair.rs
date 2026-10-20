//! Re-pairing replaces a machine only with evidence of the same computer.
use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ServerConfig};
use tessera_core::{Engine, config::Paths};
use tessera_proto::*;

async fn paired(engine: &Engine, url: &str) -> (Client, String, String, String) {
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair(
        &[url.into()],
        &BTreeMap::new(),
        &code,
        "Desktop (desktop app)",
    )
    .await
    .unwrap();
    let client = Client::connect(ServerConfig {
        endpoints: vec![url.into()],
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: grant.server_key.clone(),
    });
    client
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    (client, grant.token, grant.device_id, grant.server_key)
}

fn hello(key: Option<&str>, name: &str, scope: MachineScope) -> Request {
    Request::NodeHello {
        name: name.into(),
        os: "linux".into(),
        version: "test".into(),
        scope,
        generation: 1,
        sandbox: "on".into(),
        sandbox_ok: true,
        home: Some("/home/me".into()),
        computer: key.map(str::to_string),
        host: Some("Desktop".into()),
        features: None,
        guard_key: None,
    }
}

async fn case(
    old_key: Option<&str>,
    new_key: Option<&str>,
    replaces: bool,
    late_hello: bool,
    client: DeviceClient,
) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(engine.clone(), None).unwrap();
    let server =
        tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let (old, old_token, old_id, server_key) = paired(&engine, &url).await;
    old.call(Request::DeviceHello {
        client: DeviceClient::App,
        computer: old_key.map(str::to_string),
        host: Some("Desktop".into()),
        model: None,
    })
    .await
    .unwrap();
    // This is the login the built-in machine runs under, not a second device.
    let grant: MachineLoginGrant = old.call_as(Request::MachineLogin).await.unwrap();
    let machine = Client::connect(ServerConfig {
        endpoints: vec![url.clone()],
        headers: BTreeMap::new(),
        token: grant.token.clone(),
        server_key: server_key.clone(),
    });
    machine
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    let scope = MachineScope {
        folders: vec![FolderAccess {
            path: "/home/me/projects".into(),
            write: true,
        }],
        commands: true,
        admin: AdminAccess::Ask,
        ..Default::default()
    };
    let mut old_scope = scope.clone();
    old_scope.folders[0].write = false;
    old_scope.admin = AdminAccess::Off;
    machine
        .call(hello(old_key, "My workstation", old_scope))
        .await
        .unwrap();
    old.call(Request::SetMachinePaused {
        id: old_id.clone(),
        paused: true,
    })
    .await
    .unwrap();
    engine
        .store()
        .rename_device(&old_id, "Alex's desktop")
        .unwrap();
    engine.store().set_device_notify(&old_id, false).unwrap();
    let private: SessionInfo = old
        .call_as(Request::CreateSession {
            cwd: None,
            // Ownership only, no inference or implicit first-run model.
            model: Some("test/unused".parse().unwrap()),
            title: None,
            bot_id: None,
            private: true,
            lean: false,
        })
        .await
        .unwrap();
    let (new, _, new_id, _) = paired(&engine, &url).await;
    let device_hello = Request::DeviceHello {
        client,
        computer: new_key.map(str::to_string),
        host: Some("Desktop".into()),
        model: None,
    };
    if !late_hello {
        new.call(device_hello.clone()).await.unwrap();
    }
    // retire_login intentionally leaves the old machine until its replacement arrives.
    new.call(Request::RetireLogin {
        token: old_token.clone(),
    })
    .await
    .unwrap();
    assert!(engine.store().login(&old_token).unwrap().is_some());
    let new_grant: MachineLoginGrant = new.call_as(Request::MachineLogin).await.unwrap();
    let new_machine = Client::connect(ServerConfig {
        endpoints: vec![url.clone()],
        headers: BTreeMap::new(),
        token: new_grant.token,
        server_key,
    });
    new_machine
        .wait_connected(Duration::from_secs(10))
        .await
        .unwrap();
    let welcome: NodeWelcome = new_machine
        .call_as(hello(
            new_key,
            if replaces {
                "Desktop"
            } else {
                "My workstation"
            },
            scope.clone(),
        ))
        .await
        .unwrap();
    if late_hello {
        new.call(device_hello).await.unwrap();
    }
    let list: Vec<MachineInfo> = new.call_as(Request::ListMachines).await.unwrap();
    assert_eq!(list.len(), if replaces { 1 } else { 2 }, "{list:?}");
    let replacement = list.iter().find(|m| m.id == new_id).unwrap();
    assert!(replacement.online);
    assert_eq!(
        replacement.scope, scope,
        "folders and permissions are still the computer's own"
    );
    if replaces {
        assert_eq!(replacement.name, "My workstation");
        assert!(
            (welcome.paused || late_hello) && replacement.paused,
            "the server's pause choice carries over"
        );
        let devices = engine.store().devices(None).unwrap();
        let device = devices.iter().find(|d| d.id == new_id).unwrap();
        assert_eq!(device.name, "Alex's desktop");
        assert!(device.named && !device.notify);
        assert_eq!(
            engine.store().private_owner(&private.id),
            Some(Some(new_id.clone()))
        );
        assert!(engine.store().login(&old_token).unwrap().is_none());
        assert!(engine.store().login(&grant.token).unwrap().is_none());
        let saved = engine.store().kv_get("machines").unwrap().unwrap();
        assert!(
            !saved.contains(&old_id) && saved.contains(&new_id),
            "the replacement is persisted"
        );
        // A late disconnect from the old connection cannot take the new machine offline.
        machine.close();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let list: Vec<MachineInfo> = new.call_as(Request::ListMachines).await.unwrap();
        assert!(list[0].online);
    } else {
        assert_eq!(replacement.name, "My workstation (2)");
        assert!(!welcome.paused);
        assert!(
            list.iter()
                .any(|m| m.id == old_id && m.name == "My workstation")
        );
        assert!(engine.store().login(&old_token).unwrap().is_some());
        assert!(engine.store().login(&grant.token).unwrap().is_some());
    }
    machine.close();
    new_machine.close();
    old.close();
    new.close();
    server.abort();
}

#[tokio::test]
async fn pairing_again_on_the_same_computer_replaces_its_machine() {
    case(
        Some(&"aa".repeat(16)),
        Some(&"aa".repeat(16)),
        true,
        false,
        DeviceClient::App,
    )
    .await;
    case(
        Some(&"aa".repeat(16)),
        Some(&"aa".repeat(16)),
        true,
        true,
        DeviceClient::App,
    )
    .await;
}

#[tokio::test]
async fn different_or_unknown_computers_are_never_merged() {
    case(
        Some(&"aa".repeat(16)),
        Some(&"bb".repeat(16)),
        false,
        false,
        DeviceClient::App,
    )
    .await;
    case(None, None, false, false, DeviceClient::App).await;
    case(
        Some(&"aa".repeat(16)),
        None,
        false,
        false,
        DeviceClient::App,
    )
    .await;
    case(
        Some(&"aa".repeat(16)),
        Some("not-a-computer-key"),
        false,
        false,
        DeviceClient::App,
    )
    .await;
    // A separately paired terminal on the very same PC is not the app re-pairing.
    case(
        Some(&"aa".repeat(16)),
        Some(&"aa".repeat(16)),
        false,
        false,
        DeviceClient::Terminal,
    )
    .await;
}
