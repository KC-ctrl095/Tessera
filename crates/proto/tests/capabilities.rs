use serde_json::json;
use tessera_proto::{MachineInfo, Request};

#[test]
fn old_machine_hellos_and_records_keep_unknown_features() {
    let hello = json!({"method":"node_hello", "params": {
        "name":"PC", "os":"windows x86_64", "version":"old", "scope":{},
        "generation":1, "sandbox":"off", "sandbox_ok":false
    }});
    let req: Request = serde_json::from_value(hello.clone()).unwrap();
    assert!(matches!(req, Request::NodeHello { features: None, .. }));
    assert!(
        serde_json::to_value(req).unwrap()["params"]
            .get("features")
            .is_none()
    );
    let info = json!({"id":"pc", "name":"PC", "os":"windows x86_64", "version":"old",
        "online":true, "last_seen":0, "paused":false, "scope":{}});
    assert!(
        serde_json::from_value::<MachineInfo>(info.clone())
            .unwrap()
            .features
            .is_none()
    );
    for features in [json!([]), json!(["desktop", "terminal", "future"])] {
        let mut new = hello.clone();
        new["params"]["features"] = features.clone();
        let req: Request = serde_json::from_value(new).unwrap();
        assert_eq!(
            serde_json::to_value(req).unwrap()["params"]["features"],
            features
        );
        let mut new = info.clone();
        new["features"] = features.clone();
        let machine: MachineInfo = serde_json::from_value(new).unwrap();
        assert!(machine.features.is_some());
        assert_eq!(serde_json::to_value(machine).unwrap()["features"], features);
    }
}
