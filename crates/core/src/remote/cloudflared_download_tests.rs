//! HTTPS-only, bounded cloudflared redirects using the production client factory.
use super::*;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::{self, pki_types::*};

// Self-signed localhost certificate and TEST-ONLY key, valid through 2126.
// This key grants no access to any real service. Never included in release builds.
const CERT: &str = concat!(
    "MIIBkjCCATigAwIBAgIUeFiqHw7Dc+D6uNvviXiZn6XHpq8wCgYIKoZIzj0EAwIwFDESMBAG",
    "A1UEAwwJbG9jYWxob3N0MCAXDTI2MTAxMzA1MzYyMloYDzIxMjYwOTE5MDUzNjIyWjAUMRIw",
    "EAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARONpohnC5WxGZ7",
    "XaTPQ6IMHUCjnsoXaycgRbg6FI5XS9MK90UfWgGc4ANCRWS16m9VyFcav0Fz6/YhOkYagCcb",
    "o2YwZDAdBgNVHQ4EFgQUkb8BQ9HZabSTg9YuX5nSey8l2cowHwYDVR0jBBgwFoAUkb8BQ9HZ",
    "abSTg9YuX5nSey8l2cowFAYDVR0RBA0wC4IJbG9jYWxob3N0MAwGA1UdEwEB/wQCMAAwCgYI",
    "KoZIzj0EAwIDSAAwRQIgAooQ3+QNDwedguopZzPAsognAgtFTV0vX3yeGC0N3pYCIQDt4Cwa",
    "6nyioWwCDiC6fwiRDtHSKNzr/wTRmAgPDcvNzQ==",
);
const KEY: &str = concat!(
    "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgn9bLBdKzJy7TV+h8cjeHom4N",
    "MPSyZROhL4ldHskocg+hRANCAARONpohnC5WxGZ7XaTPQ6IMHUCjnsoXaycgRbg6FI5XS9MK",
    "90UfWgGc4ANCRWS16m9VyFcav0Fz6/YhOkYagCcb",
);
const BINARY: &str = "fixture cloudflared, never executed";

type Requests = Arc<Mutex<Vec<String>>>;

async fn https_fixture() -> (String, Requests, tokio::task::JoinHandle<()>) {
    let cert = CertificateDer::from(STANDARD.decode(CERT).unwrap());
    let key = PrivateKeyDer::Pkcs8(STANDARD.decode(KEY).unwrap().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let tls = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let requests: Requests = Arc::default();
    let (root, calls) = (base.clone(), requests.clone());
    let task = tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = tls.accept(tcp).await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let byte = stream.read_u8().await.unwrap();
                request.push(byte);
                assert!(request.len() < 16_384);
            }
            let request = String::from_utf8(request).unwrap();
            let path = request.split_whitespace().nth(1).unwrap();
            calls.lock().push(request.clone());
            let redirect = |to: &str| {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            };
            let response = match path {
                "/release" | "/bad-release" => {
                    let digest = if path == "/bad-release" {
                        "0".repeat(64)
                    } else {
                        hex(&Sha256::digest(BINARY))
                    };
                    let body = json!({"assets": [{"name": "fixture", "digest": format!("sha256:{digest}"), "browser_download_url": format!("{root}/asset")}]}).to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                "/asset" => redirect(&format!("{root}/binary")),
                "/binary" => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BINARY}",
                    BINARY.len()
                ),
                "/downgrade" => redirect(&root.replace("https://", "http://")),
                "/credentials" => redirect(&root.replace("https://", "https://user:password@")),
                "/loop" => redirect(&format!("{root}/loop")),
                _ => panic!("unexpected fixture path: {path}"),
            };
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
    });
    (base, requests, task)
}

#[tokio::test]
async fn cloudflared_download_follows_https_redirects_and_still_checks_the_digest() {
    let (base, requests, task) = https_fixture().await;
    // Exact factory selected by ensure_cloudflared. Only trust the fixture's
    // certificate and bypass proxy environment settings, not the redirect rules.
    let http = cloudflared_client()
        .add_root_certificate(
            reqwest::Certificate::from_der(&STANDARD.decode(CERT).unwrap()).unwrap(),
        )
        .no_proxy()
        .build()
        .unwrap();
    let data = tempfile::tempdir().unwrap();
    let installed = download_cloudflared(
        &http,
        &format!("{base}/release"),
        "fixture",
        data.path(),
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(installed).unwrap(), BINARY.as_bytes());
    let bad = tempfile::tempdir().unwrap();
    let error = download_cloudflared(
        &http,
        &format!("{base}/bad-release"),
        "fixture",
        bad.path(),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("doesn't match its published checksum"),
        "{error}"
    );
    assert!(
        !bad.path().join("bin").exists(),
        "nothing unverified installed"
    );
    assert_eq!(
        requests.lock().len(),
        6,
        "both downloads traverse the redirect"
    );
    for path in ["downgrade", "credentials", "loop"] {
        let before = requests.lock().len();
        assert!(
            http.get(format!("{base}/{path}")).send().await.is_err(),
            "{path}"
        );
        let count = requests.lock().len() - before;
        assert_eq!(count, if path == "loop" { 6 } else { 1 }, "{path}");
    }
    for request in requests.lock().iter() {
        let lower = request.to_ascii_lowercase();
        assert!(
            !lower.contains("authorization:") && !lower.contains("cookie:"),
            "{request}"
        );
    }
    task.abort();
}
