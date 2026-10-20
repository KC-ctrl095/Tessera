//! Settings → Devices: the programs on one computer (its desktop app, its
//! terminal, its machine) show as one entry, the same for every client; the
//! user's names stick; a machine's own name changes only on that computer.

use std::collections::BTreeMap;
use std::time::Duration;

use tessera_client::{Client, ServerConfig};
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

/// Pair a device named `name`; its login.
async fn pair(
    engine: &tessera_core::Engine,
    url: &str,
    name: &str,
    machine: bool,
) -> (String, ServerConfig) {
    let (code, _) = engine.store().create_pairing_code(60_000).unwrap();
    let (_, grant) = tessera_client::pair_as(
        std::slice::from_ref(&url.to_string()),
        &BTreeMap::new(),
        &code,
        name,
        machine,
    )
    .await
    .unwrap();
    (
        grant.device_id,
        ServerConfig {
            endpoints: vec![url.to_string()],
            headers: BTreeMap::new(),
            token: grant.token,
            server_key: grant.server_key,
        },
    )
}

async fn connected(config: ServerConfig) -> Client {
    let c = Client::connect(config);
    c.wait_connected(Duration::from_secs(10)).await.unwrap();
    c
}

fn node_hello(name: &str, computer: Option<&str>, host: Option<&str>) -> Request {
    Request::NodeHello {
        name: name.into(),
        os: "linux x86_64".into(),
        version: "test".into(),
        scope: MachineScope::default(),
        generation: 1,
        sandbox: "on".into(),
        sandbox_ok: true,
        home: None,
        computer: computer.map(str::to_string),
        host: host.map(str::to_string),
        features: None,
        guard_key: None,
    }
}

fn device_hello(
    client: DeviceClient,
    computer: Option<&str>,
    host: Option<&str>,
    model: Option<&str>,
) -> Request {
    Request::DeviceHello {
        client,
        computer: computer.map(str::to_string),
        host: host.map(str::to_string),
        model: model.map(str::to_string),
    }
}

const DESKTOP: &str = "0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn one_computer_one_entry_and_names_that_stick() {
    let (engine, url, _dir) = start().await;
    // Desktop: the desktop app, and the terminal whose machine side runs there.
    let (app_id, app_login) = pair(&engine, &url, "Linux desktop", false).await;
    let (term_id, term_login) = pair(&engine, &url, "Terminal on Desktop", false).await;
    let (phone_id, phone_login) = pair(&engine, &url, "folding phone", false).await;
    let app = connected(app_login).await;
    let term = connected(term_login.clone()).await;
    let phone = connected(phone_login).await;

    // Older programs say nothing: three entries, as before.
    let list: Vec<DeviceInfo> = app.call_as(Request::ListDevices).await.unwrap();
    let computer = |l: &[DeviceInfo], id: &str| {
        l.iter()
            .find(|d| d.id == id)
            .unwrap()
            .computer
            .clone()
            .unwrap()
    };
    assert_ne!(computer(&list, &app_id), computer(&list, &term_id));
    assert_eq!(
        list.iter().find(|d| d.id == term_id).unwrap().client,
        DeviceClient::Terminal,
        "guessed from its name"
    );

    // The app says which computer it is on; the terminal's machine side does too.
    let me: DeviceInfo = app
        .call_as(device_hello(
            DeviceClient::App,
            Some(DESKTOP),
            Some("Desktop"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        (me.id.as_str(), me.name.as_str(), me.client),
        (app_id.as_str(), "Linux desktop", DeviceClient::App)
    );
    let machine = connected(tessera_client::machine_login(&term_login).await.unwrap()).await;
    let _: NodeWelcome = machine
        .call_as(node_hello("Desktop", Some(DESKTOP), Some("Desktop")))
        .await
        .unwrap();
    // The phone app on a Fold.
    let _: DeviceInfo = phone
        .call_as(device_hello(
            DeviceClient::Phone,
            None,
            None,
            Some("Galaxy Z Fold"),
        ))
        .await
        .unwrap();

    // One computer, seen the same from every device.
    for c in [&app, &phone] {
        let list: Vec<DeviceInfo> = c.call_as(Request::ListDevices).await.unwrap();
        assert_eq!(
            computer(&list, &app_id),
            app_id,
            "named by the first paired"
        );
        assert_eq!(computer(&list, &term_id), app_id);
        assert_eq!(computer(&list, &phone_id), phone_id);
        let fold = list.iter().find(|d| d.id == phone_id).unwrap();
        assert_eq!(
            (fold.name.as_str(), fold.model.as_deref(), fold.client),
            ("folding phone", Some("Galaxy Z Fold"), DeviceClient::Phone)
        );
        assert!(
            serde_json::to_string(&list)
                .unwrap()
                .find(DESKTOP)
                .is_none(),
            "the key stays on the server"
        );
    }

    // Saying hello again never renames anything.
    let _: DeviceInfo = phone
        .call_as(device_hello(
            DeviceClient::Phone,
            None,
            None,
            Some("Galaxy Z Fold"),
        ))
        .await
        .unwrap();

    // Renaming from the phone: the whole computer takes the name; the
    // machine keeps the one it gave itself.
    let list: Vec<DeviceInfo> = phone
        .call_as(Request::RenameDevice {
            id: term_id.clone(),
            name: "  Alex's PC ".into(),
        })
        .await
        .unwrap();
    for id in [&app_id, &term_id] {
        let d = list.iter().find(|d| &d.id == id).unwrap();
        assert_eq!((d.name.as_str(), d.named), ("Alex's PC", true));
    }
    assert_eq!(
        list.iter().find(|d| d.id == phone_id).unwrap().name,
        "folding phone"
    );
    let machines: Vec<MachineInfo> = phone.call_as(Request::ListMachines).await.unwrap();
    assert_eq!(
        machines.iter().find(|m| m.id == term_id).unwrap().name,
        "Desktop"
    );
    // Reconnecting keeps the user's name.
    let _: DeviceInfo = term
        .call_as(device_hello(
            DeviceClient::Terminal,
            Some(DESKTOP),
            Some("Desktop"),
            None,
        ))
        .await
        .unwrap();
    let list: Vec<DeviceInfo> = phone.call_as(Request::ListDevices).await.unwrap();
    assert_eq!(
        list.iter().find(|d| d.id == term_id).unwrap().name,
        "Alex's PC"
    );

    // Only that computer renames its machine (`tessera node name`).
    let _ = machine
        .call(Request::NodeScope {
            scope: MachineScope::default(),
            name: Some("Alex's PC".into()),
        })
        .await
        .unwrap();
    let machines: Vec<MachineInfo> = phone.call_as(Request::ListMachines).await.unwrap();
    assert_eq!(
        machines.iter().find(|m| m.id == term_id).unwrap().name,
        "Alex's PC"
    );

    // Names the user can see and trust: one line, visible, 1 to 60 characters.
    for bad in [
        "",
        "   ",
        "two\nlines",
        "right\u{202e}left",
        &"x".repeat(61),
    ] {
        let err = phone
            .call(Request::RenameDevice {
                id: phone_id.clone(),
                name: bad.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{bad:?}");
    }
    let err = phone
        .call(Request::RenameDevice {
            id: "nope".into(),
            name: "Ghost".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    for c in [app, term, phone, machine] {
        c.close();
    }
}

#[tokio::test]
async fn a_machine_alone_is_renamed_only_on_it_and_older_ones_join_only_when_sure() {
    let (engine, url, _dir) = start().await;
    // The laptop: a new desktop app, and a machine paired by link under the
    // computer's own name by an older `tessera` (it says nothing).
    let (app_id, app_login) = pair(&engine, &url, "Dell Laptop", false).await;
    let (node_id, node_login) = pair(&engine, &url, "DESKTOP-EXAMPLE", true).await;
    let app = connected(app_login).await;
    let node = connected(node_login).await;
    let _: NodeWelcome = node
        .call_as(node_hello("DESKTOP-EXAMPLE", None, None))
        .await
        .unwrap();
    // A machine on its own: its name is that computer's to change.
    let err = app
        .call(Request::RenameDevice {
            id: node_id.clone(),
            name: "Laptop".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("only on that computer"),
        "{}",
        err.message
    );

    let _: DeviceInfo = app
        .call_as(device_hello(
            DeviceClient::App,
            Some("fedcba9876543210fedcba9876543210"),
            Some("DESKTOP-EXAMPLE"),
            None,
        ))
        .await
        .unwrap();
    let list: Vec<DeviceInfo> = app.call_as(Request::ListDevices).await.unwrap();
    let get = |id: &str| list.iter().find(|d| d.id == id).unwrap();
    assert_eq!(
        get(&node_id).computer,
        get(&app_id).computer,
        "the older machine joins by the computer's name"
    );
    assert_eq!(get(&node_id).client, DeviceClient::Machine);
    // Now part of the laptop: renaming it renames the app's sign-in, never the machine's.
    let list: Vec<DeviceInfo> = app
        .call_as(Request::RenameDevice {
            id: node_id.clone(),
            name: "Kitchen laptop".into(),
        })
        .await
        .unwrap();
    let get = |id: &str| list.iter().find(|d| d.id == id).unwrap();
    assert_eq!(get(&app_id).name, "Kitchen laptop");
    assert_eq!(get(&node_id).name, "DESKTOP-EXAMPLE");

    // A second computer reporting the same name: the older machine can't
    // tell which one it belongs to, so it stands alone.
    let (other_id, other_login) = pair(&engine, &url, "Other laptop", false).await;
    let other = connected(other_login).await;
    let _: DeviceInfo = other
        .call_as(device_hello(
            DeviceClient::App,
            Some("00112233445566778899aabbccddeeff"),
            Some("DESKTOP-EXAMPLE"),
            None,
        ))
        .await
        .unwrap();
    let list: Vec<DeviceInfo> = app.call_as(Request::ListDevices).await.unwrap();
    let get = |id: &str| list.iter().find(|d| d.id == id).unwrap();
    assert_eq!(get(&node_id).computer.as_deref(), Some(node_id.as_str()));
    assert_ne!(get(&app_id).computer, get(&other_id).computer);
    for c in [app, node, other] {
        c.close();
    }
}
