//! `docs`: Tessera's own documentation, read on demand so it never sits in
//! the prompt.

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolKind, ToolOutput, opt_str};

macro_rules! topic {
    ($name:literal, $about:literal) => {
        (
            $name,
            $about,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/docs/agent/",
                $name,
                ".md"
            )),
        )
    };
}

/// (name, one-line summary, text)
const TOPICS: &[(&str, &str, &str)] = &[
    topic!("overview", "what Tessera is, its parts, where things live"),
    topic!(
        "whiteboards",
        "showing visuals, pages, images and web apps in a tile; asking for signatures and answers"
    ),
    topic!(
        "notes",
        "the notes library: documents the user keeps, versus whiteboards"
    ),
    topic!(
        "mosaics",
        "the tiled workspace and how tiles sync across devices"
    ),
    topic!(
        "subagents",
        "background swarms, agent types, JSON reports, worktree isolation, continuing, rewind"
    ),
    topic!("memory", "memory, skills and recall: how the agent learns"),
    topic!(
        "bots",
        "how bots work: memory, knowledge, duties, channels and messaging other bots"
    ),
    topic!("scheduling", "scheduled jobs"),
    topic!(
        "notifications",
        "notify the user on their devices, reminders, sharing files as downloads"
    ),
    topic!(
        "secrets",
        "using stored tokens and logins without seeing them"
    ),
    topic!(
        "context",
        "the task ledger, compaction, steering and the queue"
    ),
    topic!(
        "permissions",
        "permission modes, approvals, the sandbox, injection scanning"
    ),
    topic!(
        "security",
        "security settings (sandbox, folders, secrets typed in chat), what you may change, guiding the user"
    ),
    topic!(
        "models",
        "model roles (@fast, @smart…), fallback chains, prices and cost"
    ),
    topic!(
        "claude-code",
        "using the user's Claude Pro/Max plan through their own Claude Code: setup, Tessera's tools in it, approvals, limits"
    ),
    topic!(
        "web",
        "search providers and fallback, API keys, web_fetch with a prompt"
    ),
    topic!(
        "browser",
        "the browser tool: tabs and web view tiles, snapshots and refs, clicking and typing, approvals, secrets"
    ),
    topic!(
        "code",
        "edit rules (seen lines, stale-tag rebase, loose replace), diagnostics after edits, the lsp tool"
    ),
    topic!("cli", "the terminal client and `tessera` commands"),
    topic!(
        "remote-access",
        "reaching Tessera away from home: walking the user through Cloudflare or Tailscale step by step, testing, turning it off, fixing problems"
    ),
    topic!(
        "machines",
        "the user's other computers (`machines` tool): what they allow, why it can't be widened, turning one on"
    ),
    topic!(
        "tools",
        "every tool, and in small model mode or a lean start turning the others on (reading this topic does it)"
    ),
];

pub struct Docs;

impl Tool for Docs {
    fn name(&self) -> &'static str {
        "docs"
    }
    fn description(&self) -> String {
        "Tessera's own documentation, how its features work. No arguments lists topics; `topic` reads one; \
         `query` searches all."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {"type": "string"},
                "query": {"type": "string"}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, args: &Value) -> String {
        opt_str(args, "topic")
            .or(opt_str(args, "query"))
            .unwrap_or("topics")
            .to_string()
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let topic = opt_str(&args, "topic");
            let text = lookup(topic, opt_str(&args, "query"));
            // Small model mode, a lean start: reading about the tools turns them on.
            if topic.is_some_and(|t| t.trim().eq_ignore_ascii_case("tools")) {
                let state = match ctx.host.more_tools(&ctx.session_id) {
                    Some(true) => "The other tools are on from your next step (call them then).",
                    Some(false) => "The other tools are already on.",
                    None => "All of your tools are on already.",
                };
                return ToolOutput::text(format!("{state}\n\n{text}"));
            }
            ToolOutput::text(text)
        })
    }
}

fn lookup(topic: Option<&str>, query: Option<&str>) -> String {
    if let Some(t) = topic {
        let t = t.trim().trim_end_matches(".md").to_ascii_lowercase();
        return match TOPICS.iter().find(|(n, ..)| *n == t) {
            Some((_, _, text)) => text.to_string(),
            None => format!("No topic `{t}`.\n\n{}", index()),
        };
    }
    if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let words: Vec<String> = q
            .to_ascii_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut hits = Vec::new();
        for (name, _, text) in TOPICS {
            for line in text.lines() {
                let l = line.to_ascii_lowercase();
                if words.iter().all(|w| l.contains(w.as_str())) {
                    hits.push(format!("[{name}] {}", line.trim()));
                }
            }
        }
        return if hits.is_empty() {
            format!("Nothing matches `{q}`.\n\n{}", index())
        } else {
            hits.truncate(40);
            format!("{}\n\nRead a whole topic with `topic`.", hits.join("\n"))
        };
    }
    index()
}

fn index() -> String {
    let list: Vec<String> = TOPICS
        .iter()
        .map(|(n, about, _)| format!("- {n}: {about}"))
        .collect();
    format!("Topics:\n{}", list.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_list_read_and_search() {
        assert!(lookup(None, None).contains("- whiteboards:"));
        assert!(lookup(Some("whiteboards"), None).contains("three/addons"));
        assert!(lookup(Some("Secrets.md"), None).contains("$NAME_OTP"));
        assert!(lookup(Some("nope"), None).starts_with("No topic"));
        // Asked to help with remote access, an agent finds the walkthrough.
        assert!(lookup(None, None).contains("- remote-access:"));
        assert!(lookup(None, Some("cloudflare token")).contains("[remote-access]"));
        assert!(lookup(Some("remote-access"), None).contains("Zero Trust"));
        let hits = lookup(None, Some("signature png"));
        assert!(hits.contains("[whiteboards]"), "{hits}");
        // Every topic is non-trivial and names itself in its heading.
        for (name, _, text) in TOPICS {
            assert!(text.len() > 300 && text.starts_with("# "), "{name}");
        }
    }
}
