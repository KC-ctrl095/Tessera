//! `tessera updates`: app builds this server offers your devices (Settings →
//! About & updates on each device offers them with one tap).

use anyhow::{Result, bail};
use clap::Subcommand;
use tessera_core::config::Paths;
use tessera_core::updates::{Build, PLATFORMS, Updates};
use tessera_core::util::human_bytes;

#[derive(Subcommand)]
pub enum UpdatesCmd {
    /// Offer an app build to your devices (replaces the one offered for its
    /// platform). The platform is found from the file when it can be.
    Add {
        /// The Android app (.apk), the desktop app or the `tessera` program.
        file: std::path::PathBuf,
        /// android-arm64, android-x86_64, linux-x86_64-desktop,
        /// linux-aarch64-desktop, linux-x86_64-cli or linux-aarch64-cli.
        #[arg(long)]
        platform: Option<String>,
        /// Its version (default: this server's).
        #[arg(long)]
        version: Option<String>,
        /// What changed, shown on the devices.
        #[arg(long)]
        notes: Option<String>,
    },
    /// List the builds offered now.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Stop offering a build (by platform or id).
    Remove { which: String },
}

pub fn main(paths: &Paths, action: Option<UpdatesCmd>) -> Result<i32> {
    let updates = Updates::new(&paths.data_dir);
    match action.unwrap_or(UpdatesCmd::List { json: false }) {
        UpdatesCmd::Add {
            file,
            platform,
            version,
            notes,
        } => {
            let b = updates.add(
                &file,
                platform.as_deref(),
                version.as_deref(),
                notes.as_deref(),
            )?;
            println!(
                "Offered to {} devices: version {}, {}.",
                b.platform,
                b.version,
                human_bytes(b.size)
            );
            println!("SHA-256 {}", b.sha256);
            println!("Devices offer it under Settings → Help & tour when they next connect.");
            Ok(0)
        }
        UpdatesCmd::List { json } => {
            let list = updates.list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("No app builds offered. Add one: tessera updates add <file>");
                println!("Platforms: {}", PLATFORMS.join(", "));
            } else {
                for b in &list {
                    print_build(b);
                }
            }
            Ok(0)
        }
        UpdatesCmd::Remove { which } => match updates.remove(which.trim())? {
            Some(b) => {
                println!("No longer offered: {} {}.", b.platform, b.version);
                Ok(0)
            }
            None => bail!("no build offered for `{which}` (see `tessera updates list`)"),
        },
    }
}

fn print_build(b: &Build) {
    let mins = tessera_core::util::now_ms().saturating_sub(b.added_at) / 60_000;
    let when = match mins {
        0 => "just now".to_string(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 48 * 60 => format!("{} h ago", m / 60),
        m => format!("{} days ago", m / (24 * 60)),
    };
    println!(
        "{:<22} {:<10} {:>9}  added {when}  {}",
        b.platform,
        b.version,
        human_bytes(b.size),
        &b.sha256[..12.min(b.sha256.len())]
    );
    if let Some(n) = &b.notes {
        println!("{:<22} {}", "", n.lines().next().unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_lists_and_removes_builds() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::at(dir.path());
        // A tiny "program" for this kind of computer.
        let mut elf = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0".to_vec();
        elf.extend_from_slice(&62u16.to_le_bytes());
        elf.extend_from_slice(&[0u8; 200]);
        let file = dir.path().join("tessera-desktop");
        std::fs::write(&file, &elf).unwrap();
        let code = main(
            &paths,
            Some(UpdatesCmd::Add {
                file: file.clone(),
                platform: None,
                version: Some("0.2.0".into()),
                notes: Some("Updates in the app".into()),
            }),
        )
        .unwrap();
        assert_eq!(code, 0);
        let list = Updates::new(&paths.data_dir).list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].platform, "linux-x86_64-desktop");
        assert_eq!(list[0].version, "0.2.0");
        assert!(dir.path().join("updates").join(&list[0].file).is_file());
        assert_eq!(main(&paths, None).unwrap(), 0);
        assert!(
            main(
                &paths,
                Some(UpdatesCmd::Remove {
                    which: "android-arm64".into()
                })
            )
            .is_err(),
            "nothing offered there"
        );
        assert_eq!(
            main(
                &paths,
                Some(UpdatesCmd::Remove {
                    which: "linux-x86_64-desktop".into()
                })
            )
            .unwrap(),
            0
        );
        assert!(Updates::new(&paths.data_dir).list().unwrap().is_empty());
        assert!(!dir.path().join("updates").join(&list[0].file).exists());
    }
}
