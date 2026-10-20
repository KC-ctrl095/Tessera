//! Slash commands and fuzzy matching.

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub desc: &'static str,
    /// Runs without arguments (Enter in the popup executes it directly).
    pub bare: bool,
}

/// Every command, in the order `/` lists them: the chat first, then the
/// rest. A terminal chat is a coding harness; everything else lives in the
/// apps.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        args: "",
        desc: "Start a new chat here (ctrl+n)",
        bare: true,
    },
    Command {
        name: "chats",
        args: "",
        desc: "Open one of your chats (the apps' too)",
        bare: true,
    },
    Command {
        name: "split",
        args: "",
        desc: "Another chat beside this one (alt+←/→ moves between them)",
        bare: true,
    },
    Command {
        name: "close",
        args: "",
        desc: "Close this side-by-side chat; it stays in /chats",
        bare: true,
    },
    Command {
        name: "model",
        args: "[provider/model]",
        desc: "The model this chat uses",
        bare: true,
    },
    Command {
        name: "thinking",
        args: "[level|show|hide]",
        desc: "How hard the model thinks (shift+tab), or show its thinking",
        bare: true,
    },
    Command {
        name: "compact",
        args: "[focus]",
        desc: "Summarize the conversation to free up context",
        bare: true,
    },
    Command {
        name: "rewind",
        args: "",
        desc: "Go back to before one of your messages",
        bare: true,
    },
    Command {
        name: "goal",
        args: "[text|clear]",
        desc: "Set the goal the agent keeps through every summary",
        bare: true,
    },
    Command {
        name: "copy",
        args: "",
        desc: "Copy the last answer",
        bare: true,
    },
    Command {
        name: "settings",
        args: "",
        desc: "Permissions, theme, the model for new chats (F2)",
        bare: true,
    },
    Command {
        name: "help",
        args: "",
        desc: "Keys and commands (F1)",
        bare: true,
    },
    Command {
        name: "quit",
        args: "",
        desc: "Leave Tessera (ctrl+d)",
        bare: true,
    },
];

/// The widest `args` (the completion list lines them up).
pub const ARGS_W: usize = 17;

const ALIASES: &[(&str, &str)] = &[
    ("exit", "quit"),
    ("q", "quit"),
    ("clear", "new"),
    ("resume", "chats"),
    ("sessions", "chats"),
    ("open", "chats"),
    ("?", "help"),
    ("keys", "help"),
    ("vsplit", "split"),
    ("unsplit", "close"),
    ("undo", "rewind"),
    ("models", "model"),
    ("think", "thinking"),
    ("preferences", "settings"),
    ("prefs", "settings"),
    ("config", "settings"),
    ("permissions", "settings"),
    ("theme", "settings"),
];

pub fn find(name: &str) -> Option<&'static Command> {
    let name = name.trim_start_matches('/');
    let name = ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map(|(_, c)| *c)
        .unwrap_or(name);
    COMMANDS.iter().find(|c| c.name == name)
}

/// Commands matching a typed prefix (fuzzy, best first; prefix matches
/// in the table's order).
pub fn matching(typed: &str) -> Vec<&'static Command> {
    let q = typed.trim_start_matches('/');
    let mut scored: Vec<(i32, usize, &'static Command)> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let s = if c.name.starts_with(q) {
                Some(1000)
            } else {
                fuzzy(q, c.name)
            };
            s.map(|s| (s, i, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, c)| c).collect()
}

/// The command a mistyped name most likely meant (`/modle` → model).
pub fn closest(typed: &str) -> Option<&'static Command> {
    let q = typed.trim_start_matches('/').to_lowercase();
    if q.is_empty() {
        return None;
    }
    let names = COMMANDS.iter().map(|c| (c.name, c)).chain(
        ALIASES
            .iter()
            .filter_map(|(a, c)| find(c).map(|cmd| (*a, cmd))),
    );
    let best = names
        .map(|(n, c)| (edits(&q, n), c))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, _)| *d <= 1 + q.chars().count() / 4)
        .map(|(_, c)| c);
    best.or_else(|| matching(&q).first().copied())
}

/// Edit distance with adjacent swaps (optimal string alignment).
fn edits(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// Subsequence match score (higher is better); None when `q` doesn't match.
/// Rewards consecutive characters and matches at word starts.
pub fn fuzzy(q: &str, target: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = target.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut prev: Option<usize> = None;
    for qc in q.chars().flat_map(char::to_lowercase) {
        if qc == ' ' {
            continue;
        }
        let mut found = None;
        while ti < t.len() {
            if t[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let pos = found?;
        score += 1;
        if prev.is_some_and(|p| p + 1 == pos) {
            score += 5;
        }
        if pos == 0
            || matches!(
                t.get(pos.wrapping_sub(1)),
                Some(' ' | '/' | '-' | '_' | '.' | ':')
            )
        {
            score += 3;
        }
        prev = Some(pos);
    }
    // Prefer shorter targets and earlier matches.
    Some(score * 10 - (t.len() as i32).min(200) / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_aliases() {
        assert_eq!(find("/model").map(|c| c.name), Some("model"));
        assert_eq!(find("exit").map(|c| c.name), Some("quit"));
        assert_eq!(find("/?").map(|c| c.name), Some("help"));
        assert_eq!(find("/sessions").map(|c| c.name), Some("chats"));
        assert_eq!(find("/theme").map(|c| c.name), Some("settings"));
        assert!(find("/nope").is_none());
        // Nothing of the apps' tiles, bots or notes is left.
        for gone in [
            "mosaic", "bots", "notes", "board", "layout", "zoom", "usage",
        ] {
            assert!(find(gone).is_none(), "/{gone} should be gone");
        }
        // Every alias names a command that exists.
        for (a, c) in ALIASES {
            assert!(find(c).is_some(), "alias {a} → {c}");
        }
    }

    #[test]
    fn matching_prefers_prefix() {
        let m: Vec<&str> = matching("/c").iter().map(|c| c.name).collect();
        assert_eq!(&m[..4], &["chats", "close", "compact", "copy"]);
        let m: Vec<&str> = matching("/mdl").iter().map(|c| c.name).collect();
        assert_eq!(m[0], "model");
        let m: Vec<&str> = matching("/").iter().map(|c| c.name).collect();
        assert_eq!(m.len(), COMMANDS.len());
        assert_eq!(m[0], "new");
        assert!(matching("/zzz").is_empty());
    }

    #[test]
    fn typos_find_the_command() {
        assert_eq!(closest("/modle").map(|c| c.name), Some("model"));
        assert_eq!(closest("/chtas").map(|c| c.name), Some("chats"));
        assert_eq!(closest("/exti").map(|c| c.name), Some("quit"));
        assert!(closest("/zzzzzzzz").is_none());
    }

    #[test]
    fn fuzzy_scores() {
        assert!(fuzzy("gpt", "openai-codex/gpt-5.5").is_some());
        assert!(fuzzy("xyz", "openai").is_none());
        assert!(fuzzy("qw", "local/qwen3") > fuzzy("qw", "local/q-something-w"));
    }

    #[test]
    fn descriptions_fit_the_popup() {
        for c in COMMANDS {
            assert!(c.desc.chars().count() <= 64, "/{} says too much", c.name);
            assert!(c.args.chars().count() <= ARGS_W, "/{} args", c.name);
        }
    }
}
