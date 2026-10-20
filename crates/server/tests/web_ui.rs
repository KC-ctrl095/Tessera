//! The web UI from `--web`: compressed for browsers that take it, a 304 for
//! files a browser already has, everything else as before.

use std::io::Read as _;

use tessera_core::config::Paths;

#[tokio::test]
async fn the_ui_goes_out_compressed_and_cached() {
    let dir = tempfile::tempdir().unwrap();
    let engine = tessera_core::Engine::open(Paths::at(dir.path()), Default::default())
        .await
        .unwrap();
    let web = dir.path().join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<!doctype html><script type=\"module\" src=\"./assets/index-abc.js\"></script>",
    )
    .unwrap();
    let script: String = (0..400)
        .map(|i| format!("export const v{i} = \"value number {i}\";\n"))
        .collect();
    std::fs::write(web.join("assets/index-abc.js"), &script).unwrap();
    let font = vec![7u8; 20_000];
    std::fs::write(web.join("assets/font.woff2"), &font).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = tessera_server::router(engine, Some(web)).unwrap();
    tokio::spawn(async move { tessera_server::serve_on(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let get = |path: &str, gzip: bool| {
        let r = http.get(format!("{url}{path}"));
        if gzip {
            r.header("accept-encoding", "gzip, deflate, br")
        } else {
            r
        }
    };

    // A browser that takes gzip gets the script compressed, the same text.
    let r = get("/assets/index-abc.js", true).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-encoding"], "gzip");
    assert_eq!(r.headers()["vary"], "accept-encoding");
    assert_eq!(
        r.headers()["cache-control"],
        "private, max-age=31536000, immutable"
    );
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    let z = r.bytes().await.unwrap();
    assert!(
        z.len() * 4 < script.len(),
        "{} of {}",
        z.len(),
        script.len()
    );
    let mut text = String::new();
    flate2::read::GzDecoder::new(&z[..])
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, script);
    // Asked again (kept compressed): the same bytes.
    let again = get("/assets/index-abc.js", true).send().await.unwrap();
    assert_eq!(again.bytes().await.unwrap(), z);

    // One that doesn't gets it as it is.
    let r = get("/assets/index-abc.js", false).send().await.unwrap();
    assert!(r.headers().get("content-encoding").is_none());
    assert_eq!(r.text().await.unwrap(), script);

    // A browser that has it gets a 304 and nothing more.
    let r = get("/assets/index-abc.js", true)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    assert!(r.bytes().await.unwrap().is_empty());

    // Fonts and pictures aren't compressed, just sent (streamed).
    let r = get("/assets/font.woff2", true).send().await.unwrap();
    assert!(r.headers().get("content-encoding").is_none());
    assert_eq!(r.headers()["content-length"], "20000");
    assert_eq!(r.bytes().await.unwrap().to_vec(), font);

    // The page: checked each time, with its policy, compressed too.
    let r = get("/", true).send().await.unwrap();
    assert_eq!(r.headers()["cache-control"], "no-cache");
    assert!(r.headers().contains_key("content-security-policy"));
    let page_tag = r.headers()["etag"].to_str().unwrap().to_string();
    let r = get("/some/route", true)
        .header("if-none-match", &page_tag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304, "the app's routes are the page too");
    // Outside the web root: the page, never the file.
    let r = get("/../config.toml", false).send().await.unwrap();
    assert!(r.text().await.unwrap().starts_with("<!doctype html>"));
}
