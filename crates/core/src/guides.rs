//! The user guides (docs/*.md) and the terms, privacy and licence notices
//! (at the top of the source), built in so every app can show them, also
//! offline and before Tessera has a public home.

macro_rules! guide {
    ($name:literal, $title:literal, $file:literal) => {
        (
            $name,
            $title,
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/", $file)),
        )
    };
}

/// A page at the top of the source (TERMS.md…) rather than in docs/.
macro_rules! top {
    ($name:literal, $title:literal, $file:literal) => {
        (
            $name,
            $title,
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $file)),
        )
    };
}

/// (name, title, markdown)
const GUIDES: &[(&str, &str, &str)] = &[
    guide!("security", "Security", "SECURITY.md"),
    guide!(
        "remote-access",
        "Reach Tessera from anywhere",
        "REMOTE_ACCESS.md"
    ),
    guide!("claude-code", "Your Claude subscription", "CLAUDE_CODE.md"),
    guide!("machines", "Your machines", "MACHINES.md"),
    guide!(
        "notifications",
        "Notifications and files",
        "NOTIFICATIONS.md"
    ),
    guide!("voice", "Talking with Tessera", "VOICE.md"),
    guide!("windows", "Tessera on Windows", "WINDOWS.md"),
    guide!(
        "windows-start",
        "Getting started on Windows",
        "GETTING-STARTED-WINDOWS.md"
    ),
    top!("terms", "Terms of use", "TERMS.md"),
    top!("privacy", "Privacy", "PRIVACY.md"),
    top!("licences", "Open-source licences", "THIRD_PARTY_NOTICES.md"),
];

/// A guide by name: its title and markdown.
pub fn get(name: &str) -> Option<(&'static str, &'static str)> {
    GUIDES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, title, text)| (*title, *text))
}

/// The guides' names.
pub fn names() -> impl Iterator<Item = &'static str> {
    GUIDES.iter().map(|(n, _, _)| *n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_guide_is_built_in() {
        for name in super::names() {
            let (title, text) = super::get(name).unwrap();
            assert!(!title.is_empty() && text.starts_with('#'), "{name}");
        }
        assert!(super::get("nope").is_none());
        // The legal pages say what matters most on their first screen.
        let text = |name| super::get(name).unwrap().1;
        assert!(text("terms").contains("No warranty"));
        assert!(text("privacy").contains("no telemetry"));
        assert!(text("licences").contains("Apache License 2.0"));
    }
}
