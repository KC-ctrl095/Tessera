//! The web app for "Use Tessera in a web browser", built into the server
//! when `TESSERA_WEB_APP` names a built copy of it (`apps/tessera/ui/dist`;
//! the installer and the release script set it after building the app).
//! Without it the server has none built in (`tessera serve --web <dir>` can
//! still give one). Text files are stored gzip-compressed (most of the app's
//! 3 MB is text: about 0.8 MB this way) and go out as they are to browsers
//! that take gzip; the page itself is kept as it is (its security policy
//! is made from it).

use std::io::Write as _;
use std::path::{Path, PathBuf};

fn files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, base, out);
        } else if p.is_file()
            && let Ok(rel) = p.strip_prefix(base)
            && let Some(rel) = rel.to_str()
        {
            out.push((rel.replace('\\', "/"), p.clone()));
        }
    }
}

fn compressible(name: &str) -> bool {
    [
        ".js",
        ".mjs",
        ".css",
        ".svg",
        ".json",
        ".webmanifest",
        ".txt",
    ]
    .iter()
    .any(|e| name.ends_with(e))
}

/// FNV-1a: a stable tag for each file's contents (its `ETag`).
fn tag(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("\"{h:016x}-{:x}\"", data.len())
}

fn main() {
    println!("cargo:rerun-if-env-changed=TESSERA_WEB_APP");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut list = Vec::new();
    if let Some(dist) = std::env::var_os("TESSERA_WEB_APP")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        println!("cargo:rerun-if-changed={}", dist.display());
        if !dist.join("index.html").is_file() {
            panic!(
                "TESSERA_WEB_APP={} has no index.html: build the web app first (cd apps/tessera/ui && npm run build)",
                dist.display()
            );
        }
        files(&dist, &dist, &mut list);
    }
    let store = out_dir.join("web-app");
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).expect("creating the web app folder");
    let mut code = String::from("pub(crate) static BUILT: &[BuiltFile] = &[\n");
    for (i, (name, path)) in list.iter().enumerate() {
        let data = std::fs::read(path).expect("reading a web app file");
        let gz = compressible(name) && data.len() >= 1024;
        let stored = if gz {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(&data).expect("compressing");
            e.finish().expect("compressing")
        } else {
            data.clone()
        };
        let file = store.join(format!("{i}.bin"));
        std::fs::write(&file, &stored).expect("writing a web app file");
        code.push_str(&format!(
            "    BuiltFile {{ path: {name:?}, gz: {gz}, etag: {:?}, data: include_bytes!({:?}) }},\n",
            tag(&data),
            file.display().to_string()
        ));
    }
    code.push_str("];\n");
    std::fs::write(out_dir.join("web_app.rs"), code).expect("writing web_app.rs");
}
