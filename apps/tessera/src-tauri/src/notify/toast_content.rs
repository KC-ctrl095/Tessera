//! Plain text only in toast XML. No chat id, URL or executable command is
//! accepted from an activation event: the live toast owns its destination.

pub fn connection_allowed(posted: u64, current: u64, attached: bool) -> bool {
    attached && posted == current
}

#[cfg(any(windows, test))]
pub fn xml(title: &str, body: &str, silent: bool) -> String {
    let escape = |s: &str| {
        s.chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect::<String>()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>",
        escape(title),
        escape(body),
        if silent {
            "<audio silent=\"true\"/>"
        } else {
            ""
        }
    )
}

#[cfg(any(windows, test))]
pub fn tag(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn switching_servers_or_replacing_the_same_login_invalidates_posts_and_clicks() {
        let posted = 1;
        assert!(super::connection_allowed(posted, 1, true));
        assert!(!super::connection_allowed(posted, 2, false)); // detach
        assert!(!super::connection_allowed(posted, 3, true)); // other server
        assert!(!super::connection_allowed(posted, 5, true)); // original server, fresh login
    }
    #[test]
    fn markup_and_controls_stay_plain_text() {
        let xml = super::xml("<a>&\"'\0", "<image src='file://x'/>\u{1}", true);
        assert!(xml.contains("&lt;a&gt;&amp;&quot;&apos;"));
        assert!(!xml.contains("<image"));
        assert!(!xml.contains('\0') && !xml.contains('\u{1}'));
        assert!(xml.contains("<audio silent=\"true\"/>"));
        assert!(!super::xml("title", "body", false).contains("<audio"));
    }
    #[test]
    fn tags_fit_windows_and_replace_only_the_same_key() {
        assert_eq!(super::tag("chat1").len(), 16);
        assert_eq!(super::tag("chat1"), super::tag("chat1"));
        assert_ne!(super::tag("chat1"), super::tag("chat2"));
    }
}
