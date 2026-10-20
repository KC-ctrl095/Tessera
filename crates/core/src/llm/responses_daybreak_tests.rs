use super::*;
use axum::{
    Json, Router,
    routing::{get, post},
};
use std::sync::{Arc, Mutex};

async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    for port in 18760..=18769 {
        if let Ok(listener) =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
        {
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            return (format!("http://127.0.0.1:{port}/v1"), task);
        }
    }
    panic!("no free L6 test port");
}

#[test]
fn daybreak_eligibility_follows_the_listed_blue_program_not_a_model_name() {
    // (metadata, Blue listed, what turning it on sends: Ok(program) or Err)
    for (metadata, blue, on) in [
        (Value::Null, false, Ok(None)),
        (json!({}), false, Ok(None)),
        (json!({"cyber": []}), false, Ok(None)),
        (json!({"cyber": ["standard"]}), false, Err(())),
        (
            json!({"cyber": ["standard", "daybreak_red"]}),
            false,
            Err(()),
        ),
        (
            json!({"cyber": ["daybreak_blue"]}),
            true,
            Ok(Some("daybreak_blue")),
        ),
        (
            json!({"cyber": ["standard", "future_program"]}),
            false,
            Err(()),
        ),
        (
            json!({"cyber": ["standard", "daybreak_blue"]}),
            true,
            Ok(Some("daybreak_blue")),
        ),
    ] {
        let model =
            json!({"slug":"gpt-daybreak-blue-latest", "available_access_programs": metadata});
        let listed = parse_codex_models(&json!({"models": [model]}));
        let programs = &listed[0].cyber_access_programs;
        assert_eq!(daybreak_available(programs), blue, "{metadata}");
        assert_eq!(
            cyber_program(programs, true).map_err(|_| ()),
            on,
            "{metadata}"
        );
        assert!(!programs.iter().any(|p| p == "future_program"));
    }
    // Off selects standard only where the catalog offers it.
    assert_eq!(
        cyber_program(&["daybreak_blue".into()], false).unwrap(),
        None
    );
    assert_eq!(cyber_program(&[], false).unwrap(), None);
    assert_eq!(
        cyber_program(&["standard".into()], false).unwrap(),
        Some("standard")
    );
}

#[tokio::test]
async fn daybreak_models_discovery_reads_caller_specific_programs_from_fake_backend() {
    let app = Router::new().route("/v1/models", get(|headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer fixture");
        Json(json!({"models": [
            {"slug":"gpt-6-sol", "available_access_programs":{"cyber":["standard","daybreak_blue"]}},
            {"slug":"gpt-daybreak-blue-latest"},
            {"slug":"hidden", "visibility":"hide", "available_access_programs":{"cyber":["daybreak_blue"]}}
        ]}))
    }));
    let (url, task) = server(app).await;
    let models = codex_models(
        &reqwest::Client::new(),
        &url,
        &[("authorization".into(), "Bearer fixture".into())],
    )
    .await
    .unwrap();
    task.abort();
    assert_eq!(models.len(), 2);
    assert!(daybreak_available(&models[0].cyber_access_programs));
    assert!(!daybreak_available(&models[1].cyber_access_programs));
}

#[tokio::test]
async fn daybreak_on_off_requests_use_official_body_and_keep_model_on_both_endpoints() {
    let seen = Arc::new(Mutex::new(Vec::<(axum::http::HeaderMap, Value)>::new()));
    let capture = seen.clone();
    let app = Router::new().route("/v1/responses", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
        capture.lock().unwrap().push((headers, body));
        async { ([ ("content-type", "text/event-stream") ], "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n") }
    }));
    let (url, task) = server(app).await;
    let http = reqwest::Client::new();
    let programs = vec!["standard".into(), "daybreak_blue".into()];
    for codex in [false, true] {
        for enabled in [false, true] {
            let ep = Endpoint {
                http: &http,
                base_url: &url,
                headers: &[],
                reasoning: true,
                vision: true,
                codex,
                fast: false,
                cyber: cyber_program(&programs, enabled).unwrap(),
                limits: None,
            };
            let mut req = tests::request("gpt-6-sol");
            req.daybreak = enabled;
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            stream(&ep, &req, &tx, &CancellationToken::new())
                .await
                .unwrap();
        }
    }
    // An unknown catalog preserves the existing shape rather than inventing access.
    let ep = Endpoint {
        http: &http,
        base_url: &url,
        headers: &[],
        reasoning: false,
        vision: false,
        codex: true,
        fast: false,
        cyber: None,
        limits: None,
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    stream(
        &ep,
        &tests::request("gpt-6-sol"),
        &tx,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    task.abort();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 5);
    for (index, (headers, body)) in requests.iter().enumerate() {
        assert_eq!(body["model"], "gpt-6-sol");
        assert!(body.get("daybreakEnabled").is_none());
        assert!(body.get("cyberAccessProgram").is_none());
        assert!(!headers.keys().any(|h| h.as_str().contains("daybreak")));
        if index == 4 {
            assert!(body.get("access_programs").is_none());
        } else {
            assert_eq!(
                body["access_programs"],
                json!({"cyber": if index % 2 == 0 {"standard"} else {"daybreak_blue"}})
            );
        }
    }
}

#[tokio::test]
async fn daybreak_backend_denial_is_surfaced_without_fallback() {
    let (url, task) = server(Router::new().route("/v1/responses", post(|| async {
        (axum::http::StatusCode::FORBIDDEN, Json(json!({"error":{"code":"access_program_not_enabled", "message":"Daybreak Blue is not enabled for this project."}})))
    }))).await;
    let http = reqwest::Client::new();
    let ep = Endpoint {
        http: &http,
        base_url: &url,
        headers: &[],
        reasoning: false,
        vision: false,
        codex: false,
        fast: false,
        cyber: Some("daybreak_blue"),
        limits: None,
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let error = stream(
        &ep,
        &tests::request("gpt-6-sol"),
        &tx,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    task.abort();
    assert_eq!(error.kind, super::super::ErrorKind::Auth);
    assert!(error.message.contains("Daybreak Blue is not enabled"));
}
