//! `tessera guard`: Tessera Guard on Windows (see `tessera_core::guard`).
//! Installing and removing it ask Windows for an administrator's approval
//! (UAC); the raised step is this same program with `install-elevated` or
//! `uninstall-elevated`. A Resume raises the guard's own protected copy
//! (`resume`, from `tessera node resume`, the Stop bar or the app). The
//! service runs `guard service`.

use anyhow::Result;
use clap::Subcommand;
use tessera_core::config::Paths;

#[derive(Subcommand)]
pub enum GuardCmd {
    /// Whether it is installed and running, and the Stop it keeps.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Install it (Windows asks an administrator).
    Install,
    /// Remove it (Windows asks an administrator).
    Uninstall,
    /// The service itself (Windows starts it).
    #[command(hide = true)]
    Service,
    #[command(hide = true)]
    InstallElevated,
    #[command(hide = true)]
    UninstallElevated,
    /// Let go of keys and buttons here (the guard starts it in the user's
    /// session after ending a machine there).
    #[command(hide = true)]
    ReleaseInput,
    /// Resume (run raised, from the guard's own program): the Stop the
    /// guard numbered `generation` under its key `key`, if approved before
    /// `until` (ms).
    #[command(hide = true)]
    Resume {
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        until: u64,
    },
}

/// The guard service's own log: `guard.log` in its folder under Program
/// Files (users may read it, only administrators change it; the service has
/// no console). The one before is kept as `guard.old.log` once it passes
/// 1 MiB, and the file never grows past 5 MiB (a flood of requests, or a
/// reader keeping it from being moved, can't fill the disk). `None` outside
/// Windows, or when it can't be opened (the guard then says why it can't
/// sign in its answers too).
pub fn service_log() -> Option<ServiceLog> {
    #[cfg(windows)]
    {
        const MOVE_AT: u64 = 1024 * 1024;
        const MOST: u64 = 5 * 1024 * 1024;
        let dir = tessera_core::guard::windows::guard_dir().ok()?;
        let path = dir.join("guard.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MOVE_AT) {
            // Kept from being moved (a reader holding it): the size bound
            // below still holds.
            let _ = std::fs::rename(&path, dir.join("guard.old.log"));
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        let size = file.metadata().map_or(MOST, |m| m.len());
        Some(ServiceLog {
            file,
            left: MOST.saturating_sub(size),
        })
    }
    #[cfg(not(windows))]
    None
}

/// A log file that takes so many bytes and then drops what comes (whole
/// lines).
#[cfg_attr(not(windows), allow(dead_code))]
pub struct ServiceLog {
    file: std::fs::File,
    left: u64,
}

impl std::io::Write for ServiceLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            self.left = 0;
            return Ok(buf.len());
        }
        // Counted before it is written (a write that fails half way counts
        // whole).
        self.left -= buf.len() as u64;
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub fn main(paths: &Paths, action: GuardCmd) -> Result<i32> {
    #[cfg(windows)]
    {
        imp::main(paths, action)
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        match action {
            GuardCmd::Status { json: true } => {
                println!("{}", serde_json::json!({ "supported": false }));
                Ok(0)
            }
            _ => {
                eprintln!(
                    "Tessera Guard is for Windows; elsewhere the user's Stop and agents' commands are kept apart by Tessera's sandbox."
                );
                Ok(2)
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::{Context, bail};
    use std::time::Duration;
    use tessera_core::guard::{self, Request, windows as win};

    pub fn main(paths: &Paths, action: GuardCmd) -> Result<i32> {
        match action {
            GuardCmd::Status { json } => status(json),
            GuardCmd::Install => raise("install-elevated", "installed"),
            GuardCmd::Uninstall => raise("uninstall-elevated", "removed"),
            GuardCmd::ReleaseInput => {
                if !tessera_core::node::screen::release_down() {
                    eprintln!("Windows refused to let go of some keys or buttons.");
                    return Ok(1);
                }
                Ok(0)
            }
            GuardCmd::Service => {
                win::run_service().context("running as the Tessera Guard service")?;
                Ok(0)
            }
            GuardCmd::InstallElevated => {
                // The Stop as this computer keeps it now, for a guard that
                // has none yet (the user approved this step).
                let local = tessera_core::node::StopLatch::load_local(paths);
                let exe = win::install(guard::State {
                    stopped: local.stopped,
                    stops: local.stops,
                    generation: 0,
                })
                .context("installing Tessera Guard")?;
                println!(
                    "Tessera Guard is installed ({}) and running.",
                    exe.display()
                );
                if let Err(why) = guard::uac_ok(win::uac(), true) {
                    println!("Commands and desktop control stay off until this is changed: {why}.");
                }
                Ok(0)
            }
            GuardCmd::UninstallElevated => {
                win::uninstall().context("removing Tessera Guard")?;
                println!("Tessera Guard is removed.");
                Ok(0)
            }
            GuardCmd::Resume {
                generation,
                key,
                until,
            } => {
                // An approval given long after it was asked for (a prompt
                // left open) resumes nothing.
                if tessera_core::util::now_ms() > until {
                    bail!("this approval came too late; press Resume again");
                }
                let r = win::ask(&Request::Resume { generation, key }, Duration::from_secs(5))
                    .context("asking Tessera Guard")?;
                if !r.ok {
                    bail!(
                        "{}",
                        r.error
                            .unwrap_or_else(|| "Tessera Guard didn't resume".into())
                    );
                }
                println!("Resumed.");
                Ok(0)
            }
        }
    }

    /// Run this program raised for `step`; Windows asks an administrator.
    fn raise(step: &str, done: &str) -> Result<i32> {
        if std::env::var_os("TESSERA").is_some_and(|v| v == "1") {
            bail!(
                "this runs inside an agent's command; Tessera Guard is installed or removed only by you, in your own terminal or the desktop app"
            );
        }
        let me = std::env::current_exe().context("finding tessera.exe")?;
        // Held while Windows asks and the raised step runs: no program can
        // change or replace it meanwhile (only reading is shared).
        let _held = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&me)
                .context("holding tessera.exe")?
        };
        let mut params = format!("guard {step}");
        if let Some(home) = std::env::var_os("TESSERA_HOME") {
            params.push_str(&format!(
                " --home \"{}\"",
                std::path::PathBuf::from(home).display()
            ));
        }
        println!("Windows asks an administrator to approve this…");
        match win::run_elevated(&me, &params, Duration::from_secs(600)) {
            Ok(0) => {
                println!("Tessera Guard is {done}.");
                Ok(0)
            }
            Ok(code) => {
                bail!("it didn't finish (exit {code}); `tessera guard status` says how things are")
            }
            Err(e) => bail!("{e}"),
        }
    }

    fn status(json: bool) -> Result<i32> {
        let s = guard::status_for(None);
        let uac = guard::uac_here();
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "supported": true,
                    "guard": s,
                    "uac_ok": uac.is_ok(),
                })
            );
            return Ok(0);
        }
        let Some(s) = s else {
            return Ok(0);
        };
        if !s.installed {
            println!(
                "Tessera Guard isn't installed: `tessera guard install` (Windows asks an administrator once)."
            );
            return Ok(3);
        }
        println!(
            "Tessera Guard: {}, {}",
            if s.running {
                "running"
            } else {
                "not answering"
            },
            if s.stopped {
                format!("stopped (Stop {})", s.stops)
            } else {
                "not stopped".into()
            }
        );
        if let Some(why) = s.blocked {
            println!("Commands and desktop control are refused: {why}");
        }
        Ok(if s.running { 0 } else { 3 })
    }
}
