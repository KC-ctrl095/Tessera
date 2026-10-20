//! Pricing in the usage stats: an Anthropic API key billed per token, a
//! ChatGPT and a Claude Code plan compared at API prices, prices from the
//! public catalog (a local stand-in), and prices and plans the user sets.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tessera_core::config::Paths;
use tessera_core::store::{Store, UsageRecord};
use tessera_core::{Engine, EngineOptions};
use tessera_proto::*;

#[derive(Default)]
struct Fake {
    /// (x-api-key, body) of each `/v1/messages` request.
    messages: Mutex<Vec<(Option<String>, Value)>>,
    catalog_hits: AtomicUsize,
}

/// A stand-in for the Anthropic API and the price catalog.
async fn fake_servers() -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                axum::Json(json!({"data": [
                    {"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "max_input_tokens": 1_000_000,
                     "max_tokens": 128_000, "capabilities": {"image_input": {"supported": true}}},
                    {"id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5", "max_input_tokens": 200_000,
                     "max_tokens": 64_000}
                ], "has_more": false}))
            }),
        )
        .route(
            "/v1/messages",
            post(
                |State(f): State<Arc<Fake>>, headers: HeaderMap, body: axum::Json<Value>| async move {
                    let key = headers
                        .get("x-api-key")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    f.messages.lock().push((key, body.0));
                    let events = [
                        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant",
                            "model": "claude-opus-5-5", "content": [], "usage": {"input_tokens": 1000,
                            "cache_read_input_tokens": 20_000, "cache_creation_input_tokens": 3000, "output_tokens": 1}}}),
                        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello from Claude"}}),
                        json!({"type": "content_block_stop", "index": 0}),
                        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 500}}),
                        json!({"type": "message_stop"}),
                    ];
                    let mut body = String::new();
                    for e in events {
                        body.push_str(&format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()));
                    }
                    ([("content-type", "text/event-stream")], body)
                },
            ),
        )
        .route(
            "/api.json",
            get(|State(f): State<Arc<Fake>>| async move {
                f.catalog_hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "openai": {"id": "openai", "models": {
                        "gpt-6.1-sol": {"id": "gpt-6.1-sol", "cost": {"input": 2.0, "output": 10.0, "cache_read": 0.1}}
                    }},
                    "reseller": {"models": {"gpt-6.1-sol": {"cost": {"input": 50.0, "output": 50.0}}}}
                }))
            }),
        )
        .route(
            "/broken.json",
            get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "down") }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

async fn call<T: serde::de::DeserializeOwned>(engine: &Engine, req: Request) -> T {
    serde_json::from_value(engine.handle(req, None).await.unwrap()).unwrap()
}

async fn usage(engine: &Engine) -> UsageReport {
    call(
        engine,
        Request::GetUsage {
            from: None,
            to: None,
            utc_offsets: Vec::new(),
        },
    )
    .await
}

fn record(
    store: &Store,
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    api_cost: Option<f64>,
) {
    let model: ModelRef = model.parse().unwrap();
    let usage = Usage {
        input,
        output,
        cache_read,
        ..Default::default()
    };
    store
        .record_usage(&UsageRecord {
            session_id: None,
            model: &model,
            kind: "chat",
            usage: &usage,
            api_cost,
        })
        .unwrap();
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-9)
}

#[tokio::test]
async fn usage_is_priced_per_token_by_plan_and_from_the_catalog() {
    let (url, fake) = fake_servers().await;
    let dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            r#"[agent]
review_interval = 0
[pricing]
catalog_url = "{url}/api.json"
[[provider]]
id = "local"
kind = "openai_chat"
base_url = "http://127.0.0.1:9/v1"
[[provider.models]]
id = "qwen"
"#
        ),
    )
    .unwrap();
    let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
        .await
        .unwrap();

    // An Anthropic API key: the model list comes with names, limits and vision.
    let info: ProviderInfo = call(
        &engine,
        Request::SaveProvider {
            spec: ProviderSpec {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: url.clone(),
                api_key: Some("test-key-not-real".into()),
                discover: true,
                models: Vec::new(),
            },
        },
    )
    .await;
    let opus = info
        .models
        .iter()
        .find(|m| m.id == "claude-opus-5-5")
        .unwrap();
    assert_eq!(opus.name, "Claude Opus 5.5");
    assert_eq!((opus.context_window, opus.max_output), (1_000_000, 128_000));
    assert!(opus.vision && opus.reasoning);

    let s: SessionInfo = call(
        &engine,
        Request::CreateSession {
            cwd: Some(work.path().display().to_string()),
            model: Some("anthropic/claude-opus-5-5".parse().unwrap()),
            title: Some("Pricing".into()),
            bot_id: None,
            private: false,
            lean: false,
        },
    )
    .await;
    let _: Value = call(
        &engine,
        Request::Send {
            session_id: s.id.clone(),
            content: vec![ContentPart::text("hi")],
            delivery: Delivery::Auto,
            attachments: Vec::new(),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(20), engine.wait_idle(&s.id))
        .await
        .unwrap();
    // The key went along; the tools + system prefix and the conversation's end
    // are both marked for caching.
    let (key, body) = fake.messages.lock()[0].clone();
    assert_eq!(key.as_deref(), Some("test-key-not-real"));
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    // Billed per token at Claude Opus 5.5's list price, cache included:
    // 1000 in × $4 + 500 out × $20 + 20k cache reads × $0.20 + 3000 writes × $5.
    let opus_cost = (1000.0 * 4.0 + 500.0 * 20.0 + 20_000.0 * 0.2 + 3000.0 * 5.0) / 1e6;
    let got: SessionSnapshot = call(
        &engine,
        Request::GetSession {
            session_id: s.id.clone(),
            since_seq: None,
        },
    )
    .await;
    assert!(
        close(got.info.usage.cost_usd, opus_cost),
        "{:?}",
        got.info.usage
    );

    // Requests through plans and a local server, as other engines record them.
    {
        let store = Store::open(&dir.path().join("tessera.db")).unwrap();
        record(
            &store,
            "openai-codex/gpt-6.1-sol",
            1_000_000,
            100_000,
            2_000_000,
            None,
        );
        // Claude Code reports its own API-equivalent cost.
        record(
            &store,
            "claude-code/claude-sonnet-5-5",
            100_000,
            10_000,
            0,
            Some(0.75),
        );
        record(&store, "local/qwen", 5000, 500, 0, None);
        record(&store, "anthropic/claude-opus-9-9", 10, 10, 0, None);
    }

    // The ChatGPT model's price isn't built in: the catalog has it.
    let r = usage(&engine).await;
    assert_eq!(fake.catalog_hits.load(Ordering::SeqCst), 1);
    assert!(dir.path().join("price-catalog.json").exists());
    let model = |r: &UsageReport, id: &str| {
        r.by_model
            .iter()
            .find(|m| format!("{}/{}", m.provider, m.model) == id)
            .cloned()
            .unwrap()
    };
    let codex = model(&r, "openai-codex/gpt-6.1-sol");
    let codex_api = 1.0 * 2.0 + 0.1 * 10.0 + 2.0 * 0.1;
    assert_eq!(codex.billing, Some(Billing::Subscription));
    assert_eq!(codex.price_source, Some(PriceSource::Catalog));
    assert!(codex.totals.cost_usd.is_none() && close(codex.totals.api_cost_usd, codex_api));
    let claude = model(&r, "anthropic/claude-opus-5-5");
    assert_eq!(
        (claude.billing, claude.price_source),
        (Some(Billing::Api), Some(PriceSource::Builtin))
    );
    assert!(
        close(claude.totals.cost_usd, opus_cost) && close(claude.totals.api_cost_usd, opus_cost)
    );
    let local = model(&r, "local/qwen");
    assert_eq!(local.billing, Some(Billing::Local));
    assert!(local.totals.api_cost_usd.is_none() && local.price.is_none());
    // Only what is billed per token is in `cost_usd`; the API view has it all.
    assert!(close(r.total.cost_usd, opus_cost));
    assert!(close(r.total.api_cost_usd, opus_cost + codex_api + 0.75));
    // An unknown model is reported, not guessed; local servers never are.
    assert_eq!(r.unpriced, vec!["anthropic/claude-opus-9-9".to_string()]);
    let plans: Vec<(&str, u64, Option<f64>)> = r
        .subscriptions
        .iter()
        .map(|s| (s.provider.as_str(), s.requests, s.api_cost_usd))
        .collect();
    assert_eq!(plans.len(), 2, "{plans:?}");
    assert!(
        plans
            .iter()
            .any(|p| p.0 == "openai-codex" && p.1 == 1 && close(p.2, codex_api))
    );
    assert!(
        plans
            .iter()
            .any(|p| p.0 == "claude-code" && p.1 == 1 && close(p.2, 0.75))
    );
    // Fetched at most daily.
    let _ = usage(&engine).await;
    assert_eq!(fake.catalog_hits.load(Ordering::SeqCst), 1);

    // The user's plan: its fee over the period against the API price.
    let _: Value = call(
        &engine,
        Request::SetSubscription {
            provider: "openai-codex".into(),
            plan: Some("  ChatGPT   Pro ".into()),
            monthly_usd: Some(200.0),
        },
    )
    .await;
    let r = usage(&engine).await;
    let plan = r
        .subscriptions
        .iter()
        .find(|s| s.provider == "openai-codex")
        .unwrap();
    assert_eq!(plan.plan.as_deref(), Some("ChatGPT Pro"));
    assert_eq!(plan.label, "ChatGPT (subscription)");
    // The fee counts from the first recorded request (moments ago here), not
    // from the start of the 30-day range.
    assert_eq!(plan.monthly_usd, Some(200.0));
    assert!(
        plan.period_usd.is_some_and(|p| (0.0..0.01).contains(&p)),
        "{plan:?}"
    );
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        saved.contains("[pricing.subscriptions.openai-codex]"),
        "{saved}"
    );

    // A price the user sets wins, also over Claude Code's own figure; clearing
    // it brings the catalog's back.
    let set = |provider: &str, model: &str, price: Option<Price>| Request::SetModelPrice {
        provider: provider.into(),
        model: model.into(),
        price,
    };
    let one = Price {
        input: 1.0,
        output: 1.0,
        ..Default::default()
    };
    let _: Value = call(&engine, set("openai-codex", "gpt-6.1-sol", Some(one))).await;
    let _: Value = call(&engine, set("claude-code", "claude-sonnet-5-5", Some(one))).await;
    let r = usage(&engine).await;
    let codex = model(&r, "openai-codex/gpt-6.1-sol");
    assert_eq!(codex.price_source, Some(PriceSource::Custom));
    // Cache reads cost the input price when not given.
    assert!(close(codex.totals.api_cost_usd, 1.0 + 0.1 + 2.0));
    assert!(close(
        model(&r, "claude-code/claude-sonnet-5-5")
            .totals
            .api_cost_usd,
        0.11
    ));
    let _: Value = call(&engine, set("openai-codex", "gpt-6.1-sol", None)).await;
    let codex = model(&usage(&engine).await, "openai-codex/gpt-6.1-sol");
    assert_eq!(codex.price_source, Some(PriceSource::Catalog));

    // Clearing a plan; nonsense is refused.
    let _: Value = call(
        &engine,
        Request::SetSubscription {
            provider: "openai-codex".into(),
            plan: None,
            monthly_usd: None,
        },
    )
    .await;
    let r = usage(&engine).await;
    let plan = r
        .subscriptions
        .iter()
        .find(|s| s.provider == "openai-codex")
        .unwrap();
    assert!(plan.plan.is_none() && plan.period_usd.is_none());
    for bad in [
        Request::SetSubscription {
            provider: "nope".into(),
            plan: Some("x".into()),
            monthly_usd: None,
        },
        Request::SetSubscription {
            provider: "openai-codex".into(),
            plan: None,
            monthly_usd: Some(-5.0),
        },
        set(
            "openai-codex",
            "gpt-6.1-sol",
            Some(Price { input: -1.0, ..one }),
        ),
        set("", "m", Some(one)),
    ] {
        let e = engine.handle(bad.clone(), None).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest, "{bad:?}");
    }
}

#[tokio::test]
async fn without_the_catalog_unknown_prices_are_listed() {
    let (url, fake) = fake_servers().await;
    for (catalog, url) in [
        ("true", format!("{url}/broken.json")),
        ("true", "http://127.0.0.1:9/api.json".to_string()),
        ("false", format!("{url}/api.json")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("[pricing]\ncatalog = {catalog}\ncatalog_url = \"{url}\"\n"),
        )
        .unwrap();
        let engine = Engine::open(Paths::at(dir.path()), EngineOptions { scheduler: false })
            .await
            .unwrap();
        {
            let store = Store::open(&dir.path().join("tessera.db")).unwrap();
            record(&store, "openai-codex/gpt-6.1-sol", 1000, 100, 0, None);
        }
        let started = std::time::Instant::now();
        let r = usage(&engine).await;
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_eq!(r.unpriced, vec!["openai-codex/gpt-6.1-sol".to_string()]);
        assert!(r.total.api_cost_usd.is_none() && r.total.cost_usd.is_none());
        assert!(!dir.path().join("price-catalog.json").exists());
    }
    // Turned off: never asked.
    assert_eq!(fake.catalog_hits.load(Ordering::SeqCst), 0);
}
