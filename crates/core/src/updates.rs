//! App builds this server offers its devices: one tap to update the phone
//! and desktop apps instead of copying files around.
//!
//! The owner puts a build in the data folder with `tessera updates add
//! <file>` (agents can't: the data folder is hidden from their commands).
//! It is copied into `<data>/updates/` and recorded in `manifest.json` with
//! its platform, version, size and SHA-256. A device asks over its encrypted
//! connection whether there is a newer build for it (`get_app_update`),
//! downloads it in pieces the same way as shared files (`read_app_update`),
//! checks the SHA-256 and only then installs it (the user confirms; Android
//! also requires the app's own signing key).
//!
//! "Newer" without build numbers: every build of the same version differs
//! only by its bytes. A device sends the SHA-256 of what it runs and when it
//! was installed. It is offered the build when it runs a build this server
//! offered before, when the build's version is higher, or (same version)
//! when the build was added after the app was installed. A build of its own
//! (a developer's, one installed by hand later) is never "updated" to an
//! older one.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tessera_proto::{ApiError, AppUpdate, ErrorCode, FileChunk, Request};

/// The platforms builds are offered for: Android packages and Linux
/// programs (desktop/CLI) and the Windows x64 NSIS setup.
pub const PLATFORMS: &[&str] = &[
    "android-arm64",
    "android-x86_64",
    "linux-x86_64-desktop",
    "linux-aarch64-desktop",
    "linux-x86_64-cli",
    "linux-aarch64-cli",
    "windows-x86_64-desktop",
];

/// The most one download request returns.
const MAX_CHUNK: u32 = 1 << 20;
/// No app is this big; anything bigger is a mistake.
const MAX_SIZE: u64 = 1 << 30;
/// What is said about a build, at most.
const MAX_NOTES: usize = 2000;
/// Older builds remembered per platform (their devices are offered the new one).
const HISTORY: usize = 50;

const MANIFEST: &str = "manifest.json";

/// One offered build, as kept in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub id: String,
    pub platform: String,
    pub version: String,
    /// Its file in the updates folder (a plain name).
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Build {
    fn offer(&self) -> AppUpdate {
        AppUpdate {
            id: self.id.clone(),
            platform: self.platform.clone(),
            version: self.version.clone(),
            size: self.size,
            sha256: self.sha256.clone(),
            added_at: self.added_at,
            notes: self.notes.clone(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    builds: Vec<Build>,
    /// SHA-256 of builds offered before, by platform (newest last).
    #[serde(default)]
    history: BTreeMap<String, Vec<String>>,
}

/// The updates folder of a server.
pub struct Updates {
    dir: PathBuf,
}

impl Updates {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("updates"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn manifest(&self) -> Result<Manifest> {
        match std::fs::read(self.dir.join(MANIFEST)) {
            Ok(b) => serde_json::from_slice(&b).context("reading the updates manifest"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, m: &Manifest) -> Result<()> {
        crate::config::write_atomic(
            &self.dir.join(MANIFEST),
            &serde_json::to_vec_pretty(m)?,
            false,
        )
    }

    /// The builds offered now, one per platform.
    pub fn list(&self) -> Result<Vec<Build>> {
        let mut b = self.manifest()?.builds;
        b.sort_by(|x, y| x.platform.cmp(&y.platform));
        Ok(b)
    }

    /// Offer `file` as the build for its platform (found from the file when
    /// `platform` isn't given), replacing the one offered before. `version`
    /// defaults to this server's.
    pub fn add(
        &self,
        file: &Path,
        platform: Option<&str>,
        version: Option<&str>,
        notes: Option<&str>,
    ) -> Result<Build> {
        let meta =
            std::fs::metadata(file).with_context(|| format!("reading {}", file.display()))?;
        if !meta.is_file() {
            bail!("{} isn't a file", file.display());
        }
        if meta.len() == 0 || meta.len() > MAX_SIZE {
            bail!("{} doesn't look like an app build", file.display());
        }
        let kind = sniff(file)?;
        let platform = match platform {
            Some(p) => {
                let p = p.trim();
                if !PLATFORMS.contains(&p) {
                    bail!("unknown platform `{p}` (one of: {})", PLATFORMS.join(", "));
                }
                p.to_string()
            }
            None => guess_platform(file, &kind)?,
        };
        check_kind(&platform, &kind)?;
        let version = match version.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) if valid_version(v) => v.to_string(),
            Some(v) => bail!("`{v}` isn't a version (like 0.2.0)"),
            None => env!("CARGO_PKG_VERSION").to_string(),
        };
        let notes = notes
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| n.chars().take(MAX_NOTES).collect::<String>());

        std::fs::create_dir_all(&self.dir)?;
        // Copied in first (and hashed while copying), so what is offered is
        // exactly what was checked, whatever happens to the original.
        let tmp = self.dir.join(format!(
            ".adding-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let (size, sha256) = match copy_hashed(file, &tmp) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let ext = if platform.starts_with("android-") {
            ".apk"
        } else if platform == "windows-x86_64-desktop" {
            ".exe"
        } else {
            ""
        };
        let name = format!("{platform}-{}{ext}", &sha256[..16]);
        let mut m = self.manifest()?;
        let old = m.builds.iter().position(|b| b.platform == platform);
        if let Some(i) = old
            && m.builds[i].sha256 == sha256
        {
            // The same build again: new words or version only.
            let _ = std::fs::remove_file(&tmp);
            m.builds[i].version = version;
            m.builds[i].notes = notes;
            let b = m.builds[i].clone();
            self.save(&m)?;
            return Ok(b);
        }
        std::fs::rename(&tmp, self.dir.join(&name))?;
        let build = Build {
            id: format!("upd{}", &sha256[..24]),
            platform: platform.clone(),
            version,
            file: name,
            size,
            sha256,
            added_at: crate::util::now_ms(),
            notes,
        };
        let replaced = old.map(|i| m.builds.remove(i));
        if let Some(r) = &replaced {
            let h = m.history.entry(platform.clone()).or_default();
            h.retain(|s| *s != r.sha256 && *s != build.sha256);
            h.push(r.sha256.clone());
            let extra = h.len().saturating_sub(HISTORY);
            h.drain(..extra);
        }
        m.builds.push(build.clone());
        self.save(&m)?;
        if let Some(r) = replaced {
            self.remove_file(&r.file);
        }
        Ok(build)
    }

    /// Stop offering the build for a platform (or with this id).
    pub fn remove(&self, which: &str) -> Result<Option<Build>> {
        let mut m = self.manifest()?;
        let Some(i) = m
            .builds
            .iter()
            .position(|b| b.platform == which || b.id == which)
        else {
            return Ok(None);
        };
        let b = m.builds.remove(i);
        self.save(&m)?;
        self.remove_file(&b.file);
        Ok(Some(b))
    }

    /// Only ever a plain file directly in the updates folder.
    fn remove_file(&self, name: &str) {
        if plain_name(name) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }

    /// The build to offer a device on `platform` running `version`
    /// (`sha256` of its own build and when it was installed, when it knows).
    pub fn offer(
        &self,
        platform: &str,
        version: &str,
        sha256: Option<&str>,
        installed_at: Option<u64>,
    ) -> Result<Option<AppUpdate>, ApiError> {
        if !PLATFORMS.contains(&platform) {
            return Err(ApiError::new(
                ErrorCode::BadRequest,
                format!("unknown platform `{platform}`"),
            ));
        }
        let m = self.manifest().map_err(internal)?;
        let Some(b) = m.builds.iter().find(|b| b.platform == platform) else {
            return Ok(None);
        };
        let own = sha256.map(|s| s.trim().to_ascii_lowercase());
        if own.as_deref() == Some(b.sha256.as_str()) {
            return Ok(None);
        }
        let older_of_ours = own.as_ref().is_some_and(|s| {
            m.history
                .get(platform)
                .is_some_and(|h| h.iter().any(|x| x == s))
        });
        let newer = older_of_ours
            || match compare_versions(&b.version, version) {
                Some(std::cmp::Ordering::Greater) => true,
                Some(std::cmp::Ordering::Less) => false,
                // The same version: newer if added after the app was installed.
                _ => installed_at.is_some_and(|t| b.added_at > t),
            };
        Ok(newer.then(|| b.offer()))
    }

    /// Part of an offered build, like a shared file's.
    pub fn read(&self, id: &str, offset: u64, len: u32) -> Result<FileChunk, ApiError> {
        use base64::Engine as _;
        let gone = || ApiError::new(ErrorCode::NotFound, "this update is no longer offered");
        let m = self.manifest().map_err(internal)?;
        let b = m.builds.iter().find(|b| b.id == id).ok_or_else(gone)?;
        if !plain_name(&b.file) {
            return Err(gone());
        }
        let path = self.dir.join(&b.file);
        // A plain file inside the folder, never a link to something else.
        let plain = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
        let inside = match (
            crate::platform::canonicalize(&path),
            crate::platform::canonicalize(&self.dir),
        ) {
            (Ok(p), Ok(d)) => p.starts_with(d),
            _ => false,
        };
        if !plain || !inside {
            return Err(gone());
        }
        let mut f = std::fs::File::open(&path).map_err(|_| gone())?;
        let size = f.metadata().map_err(|_| gone())?.len();
        if size != b.size {
            return Err(gone());
        }
        if offset > size {
            return Err(ApiError::new(
                ErrorCode::BadRequest,
                "offset past the end of the file",
            ));
        }
        f.seek(SeekFrom::Start(offset)).map_err(internal)?;
        let want = len.clamp(1, MAX_CHUNK) as u64;
        let mut buf = Vec::with_capacity(want.min(size - offset) as usize);
        f.take(want).read_to_end(&mut buf).map_err(internal)?;
        let end = offset + buf.len() as u64;
        Ok(FileChunk {
            data: base64::engine::general_purpose::STANDARD.encode(&buf),
            offset,
            size,
            eof: end >= size,
        })
    }

    /// `get_app_update` and `read_app_update`.
    pub fn handle(&self, req: Request) -> Result<serde_json::Value, ApiError> {
        let v = match req {
            Request::GetAppUpdate {
                platform,
                version,
                sha256,
                installed_at,
            } => serde_json::to_value(self.offer(
                &platform,
                &version,
                sha256.as_deref(),
                installed_at,
            )?),
            Request::ReadAppUpdate { id, offset, len } => {
                serde_json::to_value(self.read(&id, offset, len)?)
            }
            _ => {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "not an update request",
                ));
            }
        };
        v.map_err(internal)
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}

/// A file name with no way out of its folder.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

/// "0.2.0", "1.4.12-beta.1": digits and dots first, nothing odd after.
fn valid_version(v: &str) -> bool {
    v.len() <= 40
        && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Compare the numeric parts of two versions (`0.10.0` > `0.9.3`); `None`
/// when either isn't one. A pre-release (`-beta`) is ignored.
pub fn compare_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let parse = |v: &str| -> Option<Vec<u64>> {
        let core = v.trim().split(['-', '+']).next()?;
        core.split('.')
            .map(|p| p.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()
            .filter(|p| !p.is_empty())
    };
    let (mut x, mut y) = (parse(a)?, parse(b)?);
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    Some(x.cmp(&y))
}

/// What a file is, from its first bytes.
#[derive(Debug, PartialEq)]
enum Kind {
    /// An Android package (a zip), with the native code folders it has.
    Apk {
        arm64: bool,
        x86_64: bool,
    },
    /// A Linux program for this CPU (`x86_64`, `aarch64`).
    Elf(&'static str),
    WindowsSetup,
    Other,
}

fn sniff(file: &Path) -> Result<Kind> {
    let mut f = std::fs::File::open(file)?;
    let mut head = [0u8; 64];
    let n = f.read(&mut head)?;
    let head = &head[..n];
    if head.starts_with(b"MZ") {
        f.seek(SeekFrom::Start(0))?;
        let mut prefix = Vec::new();
        f.take(16 << 20).read_to_end(&mut prefix)?;
        validate_windows_installer(&prefix).map_err(anyhow::Error::msg)?;
        return Ok(Kind::WindowsSetup);
    }
    if head.starts_with(b"PK\x03\x04") {
        // The zip's directory, at its end, names the native code folders in
        // plain text.
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(16 << 20)))?;
        let mut tail = Vec::new();
        f.read_to_end(&mut tail)?;
        let has = |s: &[u8]| tail.windows(s.len()).any(|w| w == s);
        return Ok(Kind::Apk {
            arm64: has(b"lib/arm64-v8a/"),
            x86_64: has(b"lib/x86_64/"),
        });
    }
    if head.len() >= 20 && head.starts_with(b"\x7fELF") {
        // e_machine, little-endian (both CPUs are).
        return Ok(Kind::Elf(match u16::from_le_bytes([head[18], head[19]]) {
            62 => "x86_64",
            183 => "aarch64",
            _ => "other",
        }));
    }
    Ok(Kind::Other)
}

fn guess_platform(file: &Path, kind: &Kind) -> Result<String> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let p = match kind {
        Kind::Apk {
            arm64: true,
            x86_64: false,
        } => "android-arm64",
        Kind::Apk {
            arm64: false,
            x86_64: true,
        } => "android-x86_64",
        Kind::Elf(cpu @ ("x86_64" | "aarch64")) => {
            let what = if name.contains("desktop") {
                "desktop"
            } else if name == "tessera" || name.starts_with("tessera-cli") {
                "cli"
            } else {
                return Err(anyhow!(
                    "is {} the desktop app or the command-line program? Say it with --platform linux-{cpu}-desktop or linux-{cpu}-cli",
                    file.display()
                ));
            };
            return Ok(format!("linux-{cpu}-{what}"));
        }
        Kind::WindowsSetup => {
            if name.contains("tessera")
                && name.contains("setup")
                && (name.contains("x64") || name.contains("x86_64"))
            {
                "windows-x86_64-desktop"
            } else {
                bail!("name this x64 NSIS setup with --platform windows-x86_64-desktop")
            }
        }
        Kind::Apk { .. } => bail!(
            "couldn't tell which phones {} is for: say it with --platform android-arm64 (or android-x86_64)",
            file.display()
        ),
        _ => bail!(
            "{} isn't an Android app, a Linux program or a Windows NSIS installer (say --platform if it is)",
            file.display()
        ),
    };
    Ok(p.into())
}

/// The file is what its platform needs.
fn check_kind(platform: &str, kind: &Kind) -> Result<()> {
    let ok = match platform {
        "windows-x86_64-desktop" => matches!(kind, Kind::WindowsSetup),
        p if p.starts_with("android-") => matches!(kind, Kind::Apk { .. }),
        p => {
            let cpu = p.split('-').nth(1).unwrap_or_default();
            matches!(kind, Kind::Elf(c) if *c == cpu)
        }
    };
    if !ok {
        bail!("that file isn't a build for {platform}");
    }
    Ok(())
}

/// Structural check, not a signature or publisher guarantee. NSIS's setup
/// stub may be i386 even for an x64 payload, so architecture belongs to the
/// explicitly selected platform (or the conventional x64 setup filename).
/// The Windows app repeats this after its SHA-256 and size checks.
pub fn validate_windows_installer(head: &[u8]) -> Result<(), String> {
    let word = |at| {
        head.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let pe = head
        .get(60..64)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .filter(|pe| *pe <= head.len().saturating_sub(26))
        .ok_or("The Windows installer has no PE header.")?;
    if !head.starts_with(b"MZ") || pe < 64 || head.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("The Windows installer is not a PE executable.".into());
    }
    let machine = word(pe + 4).ok_or("The Windows installer is incomplete.")?;
    let flags = word(pe + 22).ok_or("The Windows installer is incomplete.")?;
    let magic = word(pe + 24).ok_or("The Windows installer is incomplete.")?;
    if !matches!((machine, magic), (0x14c, 0x10b) | (0x8664, 0x20b))
        || flags & 2 == 0
        || flags & 0x2000 != 0
        || word(pe + 20).unwrap_or_default() < 2
    {
        return Err("The Windows installer isn't a supported executable.".into());
    }
    if !head
        .windows(16)
        .any(|b| b == b"\xef\xbe\xad\xdeNullsoftInst")
    {
        return Err(
            "Offer the Windows NSIS setup installer, not a portable app or another executable."
                .into(),
        );
    }
    Ok(())
}

/// Copy `from` to `to` (a new file), returning its size and SHA-256.
fn copy_hashed(from: &Path, to: &Path) -> Result<(u64, String)> {
    use std::io::Write;
    let mut src = std::fs::File::open(from)?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        size += n as u64;
        if size > MAX_SIZE {
            bail!("{} is too big for an app build", from.display());
        }
    }
    out.sync_all()?;
    Ok((size, hex(&hash.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("0.10.0", "0.9.3"), Some(Greater));
        assert_eq!(compare_versions("0.1.0", "0.1"), Some(Equal));
        assert_eq!(compare_versions("1.2.0-beta", "1.2.0"), Some(Equal));
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Some(Less));
        assert_eq!(compare_versions("dev", "0.2.0"), None);
    }

    #[test]
    fn names_stay_in_the_folder() {
        assert!(plain_name("android-arm64-0123456789abcdef.apk"));
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", ".hidden", "x\0y"] {
            assert!(!plain_name(bad), "{bad:?}");
        }
        assert!(valid_version("0.2.0") && valid_version("1.4.12-beta.1"));
        assert!(!valid_version("../0") && !valid_version("v1") && !valid_version(""));
    }
}
