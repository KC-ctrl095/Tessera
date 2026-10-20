//! The web UI given with `--web <dir>`, or the one built into the server
//! (`build.rs`, for "Use Tessera in a web browser"). Text files (scripts, styles, the
//! page) go out gzip-compressed to browsers that take it, compressed once
//! on first request and kept in memory (the whole UI is about 0.7 MB
//! compressed); everything has an `ETag`, so a browser that has a file gets
//! a 304 instead of it again; other files (fonts, pictures) are streamed
//! rather than read whole first. Files with a hash in their name are cached
//! by browsers for a year, never by a cache on the way (a tunnel's or proxy's
//! edge would go on giving them out after "Use Tessera in a web browser" is
//! switched off); `index.html` is checked each time.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;

/// Compressed files kept in memory, at most (a UI several times today's
/// still fits; beyond it the rest go out uncompressed).
const GZ_BUDGET: usize = 8 << 20;

/// Smaller files aren't worth compressing.
const GZ_MIN: u64 = 1024;

/// Files with a hash in their name: kept a year by the browser only.
const KEEP: &str = "private, max-age=31536000, immutable";

/// A file of the web app built into the server.
pub(crate) struct BuiltFile {
    /// e.g. `index.html`, `assets/index-3f9a.js`.
    pub path: &'static str,
    /// Stored gzip-compressed.
    pub gz: bool,
    pub etag: &'static str,
    pub data: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/web_app.rs"));

/// The server has a web app built in.
pub(crate) fn has_built() -> bool {
    BUILT.iter().any(|f| f.path == "index.html")
}

#[derive(Default)]
pub(crate) struct WebFiles {
    gz: Mutex<GzCache>,
}

#[derive(Default)]
struct GzCache {
    files: HashMap<PathBuf, (String, Bytes)>,
    bytes: usize,
}

fn mime(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn compressible(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "image/svg+xml" || mime == "application/json"
}

/// The browser takes gzip (and didn't turn it off with `q=0`).
fn takes_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|item| {
            let mut parts = item.split(';').map(str::trim);
            let name = parts.next().unwrap_or("");
            let off = parts.any(|p| {
                p.strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            (name.eq_ignore_ascii_case("gzip") || name == "*") && !off
        })
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::GzEncoder::new(
        Vec::with_capacity(data.len() / 3),
        flate2::Compression::best(),
    );
    e.write_all(data)?;
    e.finish()
}

/// The file's contents in pieces, as they are read.
fn stream(file: tokio::fs::File) -> Body {
    use tokio::io::AsyncReadExt as _;
    let chunks = futures::stream::unfold(file, |mut f| async move {
        let mut buf = vec![0u8; 64 << 10];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    Body::from_stream(chunks)
}

async fn read(file: &mut tokio::fs::File) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

impl WebFiles {
    pub(crate) async fn serve(
        &self,
        root: &Path,
        uri: &Uri,
        headers: &HeaderMap,
        csp: impl Fn(&str) -> String,
    ) -> Response {
        let path = uri.path().trim_start_matches('/');
        let candidate = root.join(if path.is_empty() { "index.html" } else { path });
        // The requested file and the SPA fallback share the same containment
        // check. Keep that descriptor for metadata, compression and streaming.
        let opened = tessera_core::util::open_file_in(&candidate, root)
            .or_else(|_| tessera_core::util::open_file_in(&root.join("index.html"), root));
        let Ok((file, f)) = opened else {
            return StatusCode::NOT_FOUND.into_response();
        };
        self.serve_opened(file, f, headers, csp).await
    }

    async fn serve_opened(
        &self,
        file: PathBuf,
        f: std::fs::File,
        headers: &HeaderMap,
        csp: impl Fn(&str) -> String,
    ) -> Response {
        let mut f = tokio::fs::File::from_std(f);
        let Ok(meta) = f.metadata().await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let etag = format!("\"{:x}-{:x}\"", meta.len(), modified);
        let mime = mime(&file);
        let page = mime.starts_with("text/html");
        let cache = if file.file_name().is_some_and(|n| n == "index.html") {
            "no-cache"
        } else {
            KEEP
        };
        let gz = compressible(mime);
        let mut out = HeaderMap::new();
        out.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        out.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        if let Ok(v) = HeaderValue::from_str(&etag) {
            out.insert(header::ETAG, v);
        }
        if gz {
            out.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        let fresh = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|t| t.trim().trim_start_matches("W/") == etag)
            });
        if fresh {
            return (StatusCode::NOT_MODIFIED, out).into_response();
        }
        // The page is read whole either way: its policy names its scripts.
        if page {
            let Ok(bytes) = read(&mut f).await else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Ok(v) = HeaderValue::from_str(&csp(&String::from_utf8_lossy(&bytes))) {
                out.insert(header::CONTENT_SECURITY_POLICY, v);
            }
            if gz
                && bytes.len() as u64 >= GZ_MIN
                && takes_gzip(headers)
                && let Some(z) = self.compressed(&file, &etag, bytes.clone().into()).await
            {
                out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                return (out, z).into_response();
            }
            return (out, bytes).into_response();
        }
        if gz && meta.len() >= GZ_MIN && takes_gzip(headers) {
            if let Some(z) = self.cached(&file, &etag) {
                out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                return (out, z).into_response();
            }
            let Ok(bytes) = read(&mut f).await else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Some(z) = self.compressed(&file, &etag, bytes.clone().into()).await {
                out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                return (out, z).into_response();
            }
            return (out, bytes).into_response();
        }
        out.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
        (out, stream(f)).into_response()
    }

    /// A file of the built-in web app (the page for any other path).
    pub(crate) fn serve_built(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        csp: impl Fn(&str) -> String,
    ) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        let Some(file) = BUILT
            .iter()
            .find(|f| f.path == path)
            .or_else(|| BUILT.iter().find(|f| f.path == "index.html"))
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mime = mime(Path::new(file.path));
        let mut out = HeaderMap::new();
        out.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        out.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if file.path == "index.html" {
                "no-cache"
            } else {
                KEEP
            }),
        );
        out.insert(header::ETAG, HeaderValue::from_static(file.etag));
        if file.gz {
            out.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        let fresh = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|t| t.trim().trim_start_matches("W/") == file.etag)
            });
        if fresh {
            return (StatusCode::NOT_MODIFIED, out).into_response();
        }
        if mime.starts_with("text/html")
            && let Ok(v) = HeaderValue::from_str(&csp(&String::from_utf8_lossy(file.data)))
        {
            out.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        if !file.gz {
            return (out, Bytes::from_static(file.data)).into_response();
        }
        if takes_gzip(headers) {
            out.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            return (out, Bytes::from_static(file.data)).into_response();
        }
        // A browser that doesn't take gzip (rare): unpacked for it.
        let mut plain = Vec::new();
        use std::io::Read as _;
        if flate2::read::GzDecoder::new(file.data)
            .read_to_end(&mut plain)
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        (out, plain).into_response()
    }

    fn cached(&self, file: &Path, etag: &str) -> Option<Bytes> {
        self.gz
            .lock()
            .files
            .get(file)
            .filter(|(tag, _)| tag == etag)
            .map(|(_, z)| z.clone())
    }

    /// `file` gzipped: kept from an earlier request while it is unchanged
    /// (`etag`), or compressed now (and kept, within [`GZ_BUDGET`]).
    async fn compressed(&self, file: &Path, etag: &str, data: Bytes) -> Option<Bytes> {
        if let Some(z) = self.cached(file, etag) {
            return Some(z);
        }
        let z: Bytes = tokio::task::spawn_blocking(move || gzip(&data))
            .await
            .ok()?
            .ok()?
            .into();
        let mut cache = self.gz.lock();
        if let Some((_, old)) = cache.files.remove(file) {
            cache.bytes -= old.len();
        }
        if cache.bytes + z.len() <= GZ_BUDGET {
            cache.bytes += z.len();
            cache
                .files
                .insert(file.to_path_buf(), (etag.to_string(), z.clone()));
        }
        Some(z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn escaping_files_and_spa_fallback_are_never_served() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("web");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("private.html");
        std::fs::write(&outside, "private fixture").unwrap();
        symlink(&outside, root.join("index.html")).unwrap();
        symlink(&outside, root.join("asset.html")).unwrap();
        let web = WebFiles::default();
        for path in ["/", "/missing", "/asset.html"] {
            let res = web
                .serve(&root, &path.parse().unwrap(), &HeaderMap::new(), |_| {
                    String::new()
                })
                .await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_an_authorized_file_cannot_change_its_response() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("web");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "private fixture").unwrap();
        let original = "ordinary fixture ".repeat(200);
        for (name, compressed) in [
            ("index.html", false),
            ("asset.js", true),
            ("asset.png", false),
        ] {
            let path = root.join(name);
            std::fs::write(&path, &original).unwrap();
            let (checked, file) = tessera_core::util::open_file_in(&path, &root).unwrap();
            std::fs::rename(&path, root.join(format!("saved-{name}"))).unwrap();
            symlink(&outside, &path).unwrap();
            let mut headers = HeaderMap::new();
            if compressed {
                headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
            }
            let res = WebFiles::default()
                .serve_opened(checked, file, &headers, |_| String::new())
                .await;
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            if compressed {
                use std::io::Read as _;
                let mut plain = String::new();
                flate2::read::GzDecoder::new(bytes.as_ref())
                    .read_to_string(&mut plain)
                    .unwrap();
                assert_eq!(plain, original);
            } else {
                assert_eq!(bytes, original.as_bytes());
            }
        }
    }
    #[test]
    fn reads_accept_encoding() {
        let h = |v: &'static str| {
            let mut m = HeaderMap::new();
            m.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(v));
            m
        };
        assert!(takes_gzip(&h("gzip, deflate, br, zstd")));
        assert!(takes_gzip(&h("br;q=1.0, gzip;q=0.8")));
        assert!(!takes_gzip(&h("gzip;q=0, br")));
        assert!(!takes_gzip(&h("identity")));
        assert!(!takes_gzip(&HeaderMap::new()));
        let z = gzip(&b"hello hello hello hello".repeat(100)).unwrap();
        assert!(z.len() < 200 && z.starts_with(&[0x1f, 0x8b]));
    }
}
