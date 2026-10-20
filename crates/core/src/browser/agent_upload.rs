//! Agent-selected sources. Only checked, bounded copies reach the chooser.
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tessera_proto::Attachment;

pub(crate) const MAX_AGENT_BYTES: usize = 16 << 20;
pub(crate) const MAX_AGENT_FILES: usize = 8;

#[derive(Serialize, Deserialize)]
pub(crate) struct Sources {
    pub paths: Vec<String>,
    pub files: Vec<Attachment>,
}

/// Uploads have no exceptions inside Tessera's store, even readable skills.
/// Credential folders stay forbidden even when ordinary reads were allowed.
pub(crate) fn source_path(
    path: &Path,
    protected: &crate::tools::Protected,
    home: Option<&Path>,
) -> Result<(), String> {
    let protected = crate::tools::Protected {
        roots: protected.roots.clone(),
        except: Vec::new(),
    };
    let credentials = crate::node::credential_dirs(home);
    let hidden = path.components().any(|p| {
        matches!(
            p.as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            ".ssh"
                | ".gnupg"
                | ".claude"
                | ".codex"
                | ".password-store"
                | ".kube"
                | ".docker"
                | ".aws"
                | ".azure"
        )
    });
    if protected.covers(path)
        || protected.canonical().covers(path)
        || crate::tools::claude_state_path(path)
        || crate::tools::share::secret_file(path)
        || hidden
        || credentials.iter().any(|p| {
            let p = crate::tools::Protected {
                roots: vec![p.clone(), crate::tools::canonical(p)],
                except: Vec::new(),
            };
            p.covers(path)
        })
    {
        return Err(
            "This file is in a protected or credential location and cannot be uploaded.".into(),
        );
    }
    Ok(())
}

/// Resolve before opening and check the actual descriptor again. The callback
/// applies the caller's current read policy, not any browser-wide privilege.
pub(crate) fn read_sources(
    paths: &[PathBuf],
    check: impl Fn(&Path) -> Result<(), String>,
) -> Result<Sources, String> {
    if paths.is_empty() || paths.len() > MAX_AGENT_FILES {
        return Err("Give 1 to 8 files to upload.".into());
    }
    let mut out = Sources {
        paths: Vec::new(),
        files: Vec::new(),
    };
    let mut remaining = MAX_AGENT_BYTES;
    for path in paths {
        check(path)?;
        let opened = crate::tools::file_object::Opened::open(path).map_err(|e| e.to_string())?;
        check(&opened.path)?;
        // A hard link has no unique canonical location. Refuse it rather
        // than let an allowed alias expose a protected store or key.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if opened.file.metadata().map_err(|e| e.to_string())?.nlink() != 1 {
                return Err("Upload a regular file without hard links.".into());
            }
        }
        #[cfg(windows)]
        if crate::platform::windows::link_count(&opened.file) != Some(1) {
            return Err("Upload a regular file without hard links.".into());
        }
        let name = opened
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && super::safe_name(n) == *n)
            .ok_or("Rename this file before uploading it.")?
            .to_string();
        let bytes = opened
            .read_bounded(remaining as u64)
            .map_err(|_| "Uploads need regular files totaling at most 16 MiB.")?;
        remaining -= bytes.len();
        scan(&bytes, &[])?;
        out.paths.push(opened.path.to_string_lossy().into_owned());
        out.files.push(Attachment {
            name,
            mime: "application/octet-stream".into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    Ok(out)
}

/// Scan both binary and text copies. Unlike sharing with the user, an upload
/// never has an override for saved secrets or likely credentials.
pub(crate) fn scan(bytes: &[u8], secrets: &[String]) -> Result<(), String> {
    scan_text(&String::from_utf8_lossy(bytes), secrets)?;
    let bom = bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]);
    let bytes = if bom { &bytes[2..] } else { bytes };
    for little in [true, false] {
        let high = usize::from(little);
        // Windows text exports often omit a BOM. NULs in the high-byte lane
        // of ASCII characters identify both common UTF-16 byte orders.
        let nul_pattern = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|p| p[high] == 0 && p[1 - high].is_ascii_graphic())
            .take(4)
            .count()
            == 4;
        if bom || nul_pattern {
            let words = bytes.as_chunks::<2>().0.iter().map(|&p| {
                if little {
                    u16::from_le_bytes(p)
                } else {
                    u16::from_be_bytes(p)
                }
            });
            let text: String = char::decode_utf16(words)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            scan_text(&text, secrets)?;
        }
    }
    Ok(())
}

fn scan_text(text: &str, secrets: &[String]) -> Result<(), String> {
    if crate::tools::scrub_patterns(text) != text
        || crate::tools::redact_known(text, secrets) != text
        || text.contains("PRIVATE KEY-----")
    {
        return Err("This file contains a saved secret or looks like credentials. Upload a redacted copy instead.".into());
    }
    Ok(())
}

impl Sources {
    pub(crate) fn validate(&self, secrets: &[String]) -> Result<(), String> {
        if self.files.is_empty()
            || self.files.len() > MAX_AGENT_FILES
            || self.paths.len() != self.files.len()
        {
            return Err("The upload source list is invalid.".into());
        }
        let mut remaining = MAX_AGENT_BYTES;
        for file in &self.files {
            if file.data.len() > remaining.div_ceil(3) * 4 {
                return Err("Upload at most 16 MiB in one call.".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&file.data)
                .map_err(|_| "The upload bytes are invalid.")?;
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or("Upload at most 16 MiB in one call.")?;
            scan(&bytes, secrets)?;
        }
        Ok(())
    }

    pub(crate) fn manifest(&self) -> String {
        self.paths
            .iter()
            .zip(&self.files)
            .map(|(path, file)| {
                let padding = file.data.bytes().rev().take_while(|b| *b == b'=').count();
                let size = file.data.len() / 4 * 3 - padding;
                format!("{path} ({size} bytes)")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The exact chooser and receiving frame used for this approval.
#[derive(Clone, PartialEq)]
pub(crate) struct Chooser {
    pub id: String,
    pub node: i64,
    pub many: bool,
    pub url: String,
    pub accept: String,
}

fn frame_url(tree: &serde_json::Value, frame: &str) -> Option<String> {
    if tree["frame"]["id"].as_str() == Some(frame) {
        return tree["frame"]["url"].as_str().map(str::to_string);
    }
    tree["childFrames"]
        .as_array()?
        .iter()
        .find_map(|t| frame_url(t, frame))
}

impl super::Tab {
    pub(crate) async fn upload_chooser(&self) -> Result<Chooser, String> {
        let (id, node, many, frame) = {
            let st = self.st.lock();
            let (id, node, many) = st.file_chooser.clone().ok_or(
                "No file picker is open. Click the page's Choose file or Upload button first.",
            )?;
            (id, node, many, st.file_chooser_frame.clone())
        };
        let link = self.link()?;
        let tree = link
            .cdp
            .call(
                Some(&link.session),
                "Page.getFrameTree",
                serde_json::json!({}),
            )
            .await?;
        let url = frame_url(&tree["frameTree"], &frame)
            .ok_or("The file picker's page changed. Open it again.")?;
        let dest =
            url::Url::parse(&url).map_err(|_| "The upload destination cannot be checked.")?;
        if !matches!(dest.scheme(), "http" | "https") || !dest.has_host() {
            return Err("Upload only to an http or https page.".into());
        }
        let info = link
            .cdp
            .call(
                Some(&link.session),
                "DOM.describeNode",
                serde_json::json!({"backendNodeId":node,"depth":0}),
            )
            .await?;
        let attrs = info["node"]["attributes"]
            .as_array()
            .ok_or("The file input could not be checked.")?;
        let attribute = |name: &str| {
            attrs
                .as_chunks::<2>()
                .0
                .iter()
                .find(|[key, _]| key.as_str() == Some(name))
                .and_then(|[_, value]| value.as_str())
                .map(str::to_string)
        };
        if attribute("webkitdirectory").is_some() || attribute("directory").is_some() {
            return Err(
                "Folder uploads are not supported. Choose individual files instead.".into(),
            );
        }
        if self.st.lock().file_chooser.as_ref().map(|c| &c.0) != Some(&id) {
            return Err("That file picker closed. Open it again.".into());
        }
        Ok(Chooser {
            id,
            node,
            many,
            url,
            accept: attribute("accept").unwrap_or_default(),
        })
    }

    pub(crate) fn upload_actor(&self, session: &str) -> Result<serde_json::Value, String> {
        let browser = self.host_browser().ok_or("The browser closed.")?;
        let host = browser.host()?;
        let who = host.acting_as(session);
        let stops = browser.machine().and_then(|m| host.turn_stops(session, &m));
        Ok(
            serde_json::json!({"session":session,"chat":who.chat,"bot":who.bot,"mode":who.mode,"stops":stops,"action":"upload"}),
        )
    }

    pub(crate) async fn remote_upload_sources(
        &self,
        paths: &[String],
        actor: &serde_json::Value,
    ) -> Result<Sources, String> {
        let link = self.link()?;
        let value = link
            .cdp
            .call(
                Some(&link.session),
                "Tessera.readUpload",
                serde_json::json!({"paths":paths,"actor":actor}),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|_| "The machine could not read the upload files.".into())
    }

    pub(crate) async fn send_agent_files(
        &self,
        chooser: &Chooser,
        sources: &Sources,
        actor: &serde_json::Value,
    ) -> Result<(), String> {
        if self.upload_chooser().await? != *chooser {
            return Err(
                "The page or file picker changed while approval was pending. Nothing was sent."
                    .into(),
            );
        }
        let browser = self.browser().ok_or("The browser closed.")?;
        let link = self.link()?;
        if browser.machine().is_some() {
            link.cdp.call(Some(&link.session), "Tessera.agentChooseFiles", serde_json::json!({"backendNodeId":chooser.node,"sources":sources,"actor":actor})).await?;
        } else {
            let paths = browser.0.uploads.lock().stage_agent(&sources.files)?;
            if self.st.lock().file_chooser.as_ref().map(|c| &c.0) != Some(&chooser.id) {
                return Err("The file picker closed before sending.".into());
            }
            self.link()?;
            link.cdp
                .call(
                    Some(&link.session),
                    "DOM.setFileInputFiles",
                    serde_json::json!({"backendNodeId":chooser.node,"files":paths}),
                )
                .await?;
        }
        {
            let mut st = self.st.lock();
            if st.file_chooser.as_ref().map(|c| &c.0) == Some(&chooser.id) {
                st.file_chooser = None;
            }
        }
        self.publish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_uploads_are_scanned_in_both_byte_orders_with_or_without_a_bom() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        for little in [true, false] {
            for bom in [true, false] {
                let encode = |text: &str| -> Vec<u8> {
                    let prefix = if bom {
                        vec![if little { 0xfeffu16 } else { 0xfffeu16 }]
                    } else {
                        Vec::new()
                    };
                    let mut bytes: Vec<_> = prefix.into_iter().flat_map(u16::to_le_bytes).collect();
                    bytes.extend(text.encode_utf16().flat_map(|c| {
                        if little {
                            c.to_le_bytes()
                        } else {
                            c.to_be_bytes()
                        }
                    }));
                    bytes
                };
                for text in [
                    "-----BEGIN PRIVATE KEY-----\nfixture",
                    "password=fixture-value",
                    "contains saved-fixture-value",
                ] {
                    let bytes = encode(text);
                    // The former UTF-8-only check misses each of these copies.
                    assert!(
                        scan_text(
                            &String::from_utf8_lossy(&bytes),
                            &["saved-fixture-value".into()]
                        )
                        .is_ok()
                    );
                    assert!(
                        scan(&bytes, &["saved-fixture-value".into()]).is_err(),
                        "little={little}, bom={bom}"
                    );
                }
                let bytes = encode("ordinary notes with café and 日本語");
                assert!(scan(&bytes, &[]).is_ok());
                std::fs::write(&file, &bytes).unwrap();
                let sources = read_sources(std::slice::from_ref(&file), |_| Ok(())).unwrap();
                sources.validate(&[]).unwrap();
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(&sources.files[0].data)
                        .unwrap(),
                    bytes
                );
            }
        }
    }

    #[test]
    fn upload_sources_refuse_protected_paths_links_and_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let protected = crate::tools::Protected {
            roots: vec![dir.path().join("private")],
            except: vec![dir.path().join("private/skills")],
        };
        std::fs::create_dir_all(dir.path().join("private/skills")).unwrap();
        std::fs::write(dir.path().join("private/skills/file.txt"), "notes").unwrap();
        let check = |p: &Path| source_path(p, &protected, Some(dir.path()));
        assert!(read_sources(&[dir.path().join("private/skills/file.txt")], check).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                dir.path().join("private/skills/file.txt"),
                dir.path().join("link.txt"),
            )
            .unwrap();
            assert!(read_sources(&[dir.path().join("link.txt")], check).is_err());
        }
        #[cfg(unix)]
        {
            let ordinary = dir.path().join("ordinary.txt");
            std::fs::write(&ordinary, "ordinary notes").unwrap();
            let alias = dir.path().join("hardlink.txt");
            std::fs::hard_link(&ordinary, &alias).unwrap();
            assert!(read_sources(&[alias], check).is_err());
        }
        for path in [".ssh/config", "report.key", ".env", ".gnupg/info"] {
            assert!(check(&dir.path().join(path)).is_err(), "{path}");
        }
        assert!(scan(b"normal notes", &[]).is_ok());
        assert!(
            scan(
                b"contains saved-password-value",
                &["saved-password-value".into()]
            )
            .is_err()
        );
        assert!(scan(b"-----BEGIN PRIVATE KEY-----\nfixture", &[]).is_err());
    }

    #[test]
    fn upload_sources_bound_regular_files_and_keep_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        let bytes = b"ordinary notes\0with binary bytes";
        std::fs::write(&file, bytes).unwrap();
        let sources = read_sources(std::slice::from_ref(&file), |_| Ok(())).unwrap();
        sources.validate(&[]).unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&sources.files[0].data)
                .unwrap(),
            bytes
        );
        assert!(
            sources
                .manifest()
                .contains(&format!("report.txt ({} bytes)", bytes.len()))
        );
        assert!(read_sources(&[], |_| Ok(())).is_err());
        assert!(read_sources(&vec![file.clone(); 9], |_| Ok(())).is_err());
        assert!(read_sources(&[dir.path().to_path_buf()], |_| Ok(())).is_err());
        std::fs::File::create(&file)
            .unwrap()
            .set_len(MAX_AGENT_BYTES as u64 + 1)
            .unwrap();
        assert!(read_sources(&[file], |_| Ok(())).is_err());
    }
}
