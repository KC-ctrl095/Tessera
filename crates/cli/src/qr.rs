//! QR codes for pairing links, drawn with half-block characters.

use qrcode::{Color, EcLevel, QrCode};

use crate::setup::term::Depth;

/// Modules of light margin around the code (the standard's quiet zone).
const QUIET: usize = 4;

/// Rows of text that scan as a QR code, two modules per row. With colours
/// the code is drawn in explicit black and white, so it scans on light and
/// dark terminals alike; without, the terminal's own colours are used:
/// light modules are drawn on a dark background (`light_bg`: the reverse).
pub fn render(data: &str, depth: Depth, light_bg: bool) -> Option<Vec<String>> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    let n = code.width();
    let colors = code.to_colors();
    let size = n + 2 * QUIET;
    // Light modules (the margin too) are the ones drawn, unless the
    // terminal is light and has no colours to draw them with.
    let ink = |x: usize, y: usize| -> bool {
        let light = if x < QUIET || y < QUIET || x >= n + QUIET || y >= n + QUIET {
            true
        } else {
            colors[(y - QUIET) * n + (x - QUIET)] == Color::Light
        };
        light != (depth == Depth::None && light_bg)
    };
    let (open, close) = match depth {
        Depth::None => ("", ""),
        Depth::Ansi16 => ("\x1b[97;40m", "\x1b[0m"),
        Depth::Ansi256 => ("\x1b[38;5;231;48;5;16m", "\x1b[0m"),
        Depth::True => ("\x1b[38;2;255;255;255;48;2;0;0;0m", "\x1b[0m"),
    };
    let rows = (0..size)
        .step_by(2)
        .map(|y| {
            let mut s = String::from(open);
            for x in 0..size {
                // Past the bottom edge the margin continues.
                let bottom = y + 1 >= size || ink(x, y + 1);
                s.push(match (ink(x, y), bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
            s.push_str(close);
            s
        })
        .collect();
    Some(rows)
}

/// What the owner should know about where a pairing link works: only on
/// this server's network, a Cloudflare tunnel that isn't connected (and
/// why), or another remote address that isn't answering now.
pub async fn reach_notes(
    link: &tessera_proto::PairLink,
    tunnel: Option<&tessera_proto::TunnelStatus>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !link.urls.is_empty() && link.urls.iter().all(|u| home_network_only(u)) {
        notes.push(
            "These addresses only work on this server's network. To pair a phone on another \
             network (or mobile data), set up remote access first: `tessera setup devices`."
                .to_string(),
        );
    }
    let tunnel_url = tunnel.map(|t| format!("https://{}", t.hostname));
    if let Some(t) = tunnel.filter(|t| !t.connected) {
        notes.push(match &t.error {
            Some(e) => format!(
                "https://{} won't work until the Cloudflare tunnel connects: {e}",
                t.hostname
            ),
            None => format!(
                "The Cloudflare tunnel for https://{} is still connecting; if pairing from \
                 elsewhere fails, check `tessera setup devices` in a minute.",
                t.hostname
            ),
        });
    }
    let headers: std::collections::BTreeMap<String, String> =
        link.headers.iter().cloned().collect();
    for u in link.urls.iter().filter(|u| !home_network_only(u)) {
        if tunnel.is_some() && tunnel_url.as_deref() == Some(u.trim_end_matches('/')) {
            continue;
        }
        if let Err(e) = tessera_client::probe(std::slice::from_ref(u), &headers).await {
            let why = why_unreachable(&e.to_string(), u);
            notes.push(format!(
                "{u} doesn't work right now ({why}), so devices away from home can't \
                 connect through it. Check remote access: `tessera setup devices`."
            ));
        }
    }
    notes
}

/// The reason in a probe error ("couldn't reach the server (<url>: <why>)").
pub fn why_unreachable(err: &str, url: &str) -> String {
    err.split_once(&format!("{url}: "))
        .map(|(_, w)| w.trim_end_matches(')'))
        .unwrap_or(err)
        .to_string()
}

/// An address only devices on the same network can use: a private, loopback
/// or link-local IP, or a local name (Tailscale addresses work anywhere).
fn home_network_only(url: &str) -> bool {
    use std::net::IpAddr;
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    }
    .to_ascii_lowercase();
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
        Err(_) => {
            !host.contains('.')
                || [
                    ".local",
                    ".lan",
                    ".home",
                    ".internal",
                    ".home.arpa",
                    ".localhost",
                ]
                .iter()
                .any(|s| host.ends_with(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::home_network_only;

    #[test]
    fn home_network_addresses_are_told_apart() {
        for u in [
            "http://192.168.1.20:7878",
            "http://10.0.0.5:7878/",
            "http://172.20.1.1",
            "http://127.0.0.1:7878",
            "http://[fd00::1]:7878",
            "http://my-server:7878",
            "http://my-server.local:7878",
        ] {
            assert!(home_network_only(u), "{u}");
        }
        for u in [
            "https://tessera.example.com",
            "http://100.64.0.20:7878",
            "https://my-server.tail1234.ts.net",
            "http://[2606:4700::1]:7878",
        ] {
            assert!(!home_network_only(u), "{u}");
        }
    }

    #[tokio::test]
    async fn notes_say_when_a_link_only_works_at_home() {
        let link = |urls: &[&str]| tessera_proto::PairLink {
            urls: urls.iter().map(|u| u.to_string()).collect(),
            ..Default::default()
        };
        let notes = super::reach_notes(&link(&["http://192.168.1.20:7878"]), None).await;
        assert!(
            notes.len() == 1 && notes[0].contains("only work on this server's network"),
            "{notes:?}"
        );
        // A remote address that doesn't answer is named, with the reason.
        let both = link(&["http://192.168.1.20:7878", "https://tessera.invalid"]);
        let notes = super::reach_notes(&both, None).await;
        assert!(
            notes.len() == 1
                && notes[0].starts_with(
                    "https://tessera.invalid doesn't work right now (address not found)"
                ),
            "{notes:?}"
        );
        // A tunnel Tessera runs says why it isn't connected (without probing).
        let down = tessera_proto::TunnelStatus {
            hostname: "tessera.invalid".into(),
            tunnel_id: "t".into(),
            running: true,
            connected: false,
            error: Some("outbound port 7844 looks blocked".into()),
            key_expires_at: None,
            renewable: true,
        };
        let notes = super::reach_notes(&both, Some(&down)).await;
        assert!(
            notes.len() == 1
                && notes[0].contains("until the Cloudflare tunnel connects: outbound port 7844"),
            "{notes:?}"
        );
    }

    #[test]
    fn renders_a_pairing_link() {
        use crate::setup::term::{Depth, strip_ansi};
        let link = "tessera://pair?v=1&n=Tessera&u=http%3A%2F%2F192.168.1.50%3A7878&u=https%3A%2F%2Ftessera.example.com&h=CF-Access-Client-Id%3Aabcdef0123456789abcdef0123456789.access&h=CF-Access-Client-Secret%3A0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&c=1234-5678&f=3f9a1c2b77e05d41a0c9";
        let rows = super::render(link, Depth::True, false).unwrap();
        // Fits an 80-column terminal (with the wizard's indent).
        let w = strip_ansi(&rows[0]).chars().count();
        assert!(w + 4 <= 80 && rows.len() <= 40, "{w}x{}", rows.len());
        // The margin is drawn light all round; the corner finder pattern
        // starts dark right after it.
        let top = strip_ansi(&rows[0]);
        assert!(top.chars().all(|c| c == '█'), "{top}");
        let finder = strip_ansi(&rows[2]);
        assert_eq!(finder.chars().nth(super::QUIET), Some(' '), "{finder}");
        assert!(rows[0].starts_with("\x1b[38;2;255;255;255;48;2;0;0;0m"));
        // Without colours on a light terminal the dark modules are drawn.
        let plain = super::render(link, Depth::None, true).unwrap();
        assert!(plain[0].chars().all(|c| c == ' '));
        assert_eq!(plain[2].chars().nth(super::QUIET), Some('█'));

        // Read the half blocks back: every module matches the encoder's.
        let code = qrcode::QrCode::with_error_correction_level(link.as_bytes(), qrcode::EcLevel::L)
            .unwrap();
        let (n, colors) = (code.width(), code.to_colors());
        let q = super::QUIET;
        for (r, row) in rows.iter().enumerate() {
            for (x, ch) in strip_ansi(row).chars().enumerate() {
                let (top, bottom) = match ch {
                    '█' => (true, true),
                    '▀' => (true, false),
                    '▄' => (false, true),
                    _ => (false, false),
                };
                for (y, drawn) in [(2 * r, top), (2 * r + 1, bottom)] {
                    let inside = (q..n + q).contains(&x) && (q..n + q).contains(&y);
                    let light = !inside || colors[(y - q) * n + (x - q)] == qrcode::Color::Light;
                    assert_eq!(drawn, light, "module {x},{y}");
                }
            }
        }
    }
}
