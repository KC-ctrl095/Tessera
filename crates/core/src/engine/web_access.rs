//! "Use Tessera in a web browser" (off by default; Settings → Connection).
//!
//! While on, the server gives its web app to browsers at `/web/` on its
//! https address, and browsers pair there with a code made on one of the
//! user's devices (the owner's approval). A browser is a device of its own
//! kind: it keeps a key it can't read out, proves it holds it every time it
//! connects, and can be signed out in Devices like any other. While off,
//! nothing is served at `/web/` and paired browsers can't connect; turning
//! it off closes their connections at once. The HTTP side is in
//! `tessera-server` (`web_access` there).

use std::sync::atomic::Ordering;

use super::*;

impl Engine {
    /// The server has the web app to give browsers (built into it, or
    /// `--web <folder>`); without it the switch can't be turned on.
    pub fn set_web_app(&self, has: bool) {
        self.0.web_app.store(has, Ordering::Relaxed);
    }

    /// Browsers may use Tessera now: switched on, with a web app to give.
    pub fn web_access_on(&self) -> bool {
        self.0.web_app.load(Ordering::Relaxed) && self.0.config.read().server.web_access
    }
}

impl Inner {
    /// The switch's state for Settings. `urls`: the addresses devices use
    /// from elsewhere.
    pub(super) fn web_access_status(&self, urls: &[String]) -> WebAccess {
        let tunnel = self.auth.tunnel();
        let https = urls
            .iter()
            .find(|u| u.starts_with("https://"))
            .cloned()
            .or_else(|| tunnel.as_ref().map(|t| format!("https://{}", t.hostname)));
        WebAccess {
            on: self.config.read().server.web_access,
            available: self.web_app.load(Ordering::Relaxed),
            url: https.map(|u| format!("{}/web/", u.trim_end_matches('/'))),
            needs_token: tunnel.is_some_and(|t| t.web_app_id.is_empty()),
            browsers: self.store.browsers().map_or(0, |b| b.len() as u32),
        }
    }

    /// Switch it on or off (`token`: a Cloudflare API token, when the
    /// tunnel Tessera set up must let browsers through first).
    pub(super) async fn set_web_access(
        &self,
        on: bool,
        token: Option<String>,
    ) -> Result<RemoteStatus, ApiError> {
        if on {
            if !self.web_app.load(Ordering::Relaxed) {
                return Err(bad(
                    "this server was built without the web app; install Tessera again on it with Node.js there (the installer then builds the web app too)",
                ));
            }
            if let Some(mut record) = self.auth.tunnel().filter(|t| t.web_app_id.is_empty()) {
                let token = token.as_deref().map(str::trim).unwrap_or_default();
                if token.is_empty() {
                    return Err(bad(
                        "Cloudflare lets only the Tessera apps reach this server; letting browsers reach it needs a Cloudflare API token once (the same kind as for setting up remote access)",
                    ));
                }
                record.web_app_id =
                    crate::remote::Cloudflare::new(self.providers.http().clone(), token)
                        .web_door(&record)
                        .await
                        .map_err(|e| bad(format!("Cloudflare: {e}")))?;
                self.auth.set_tunnel(Some(record)).map_err(internal)?;
            }
        }
        {
            let mut c = self.config.write();
            c.server.web_access = on;
            c.save(&self.paths).map_err(internal)?;
        }
        if !on {
            // Paired browsers stop at once: their open connections close (as
            // when one is signed out), and new ones are refused. Terminals
            // never open from a browser; should one ever have, it ends too.
            for id in self.store.browsers().unwrap_or_default() {
                let _ = self.revoked.send(id.clone());
                self.end_terminals_of(&id, "web browser access was turned off");
            }
        }
        tracing::info!(
            "use Tessera in a web browser: turned {}",
            if on { "on" } else { "off" }
        );
        Ok(self.remote_status())
    }
}
