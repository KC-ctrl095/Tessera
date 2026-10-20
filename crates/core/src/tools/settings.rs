//! `settings`: read Tessera's settings and change everyday ones (model
//! roles, memory, context, language servers…) with the user's approval,
//! instead of editing hidden files. Security, network, provider and
//! credential settings stay with the user.

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::PermissionMode;

use super::{Approve, Tool, ToolCtx, ToolKind, ToolOutput, opt_str};

/// Settings an agent may propose changing (a dotted prefix or exact key).
const SETTABLE: &[&str] = &[
    "default_model",
    "utility_model",
    "models.",
    "prices.",
    "pricing.",
    "agent.",
    "context.",
    "memory.",
    "skills.disabled",
    "lsp.",
    "web.search",
    "web.chain",
    "browser.enabled",
    "browser.headless",
    "browser.idle_minutes",
    "browser.max_tabs",
    "browser.freeze_seconds",
    "browser.scale",
    "server.name",
    // This chat's helper limits (`engine/helper_limits.rs`).
    "chat.helpers",
    "chat.helper_steps",
    "chat.helper_minutes",
];

/// Longest value (as JSON) an approval shows whole.
const MAX_VALUE: usize = 2000;

/// Inside the prefixes above, still the user's alone: the permission mode
/// new chats get, and anything naming a place requests or files go to.
const NEVER: &[&str] = &["agent.permission", "agent.pass_env"];

/// Why an agent may not change `key`, if it may not.
pub fn refusal(key: &[String]) -> Option<String> {
    let dotted = key.join(".");
    let allowed = SETTABLE.iter().any(|p| match p.strip_suffix('.') {
        Some(section) => key.len() > 1 && key[0] == section,
        None => dotted == *p,
    }) && !NEVER
        .iter()
        .any(|n| dotted == *n || dotted.starts_with(&format!("{n}.")))
        && !key.iter().any(|k| {
            let k = k.to_ascii_lowercase();
            k.contains("url") || k.contains("path") || k == "headers"
        });
    (!allowed).then(|| {
        format!(
            "`{dotted}` is not a setting agents may change (security, network, providers, credentials and permissions are the user's). \
             Tell the user where to change it: Settings in the app, or `config.toml`."
        )
    })
}

/// `models.roles.fast`, `prices."openai/gpt-5.5"` → path segments.
pub fn parse_key(key: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in key.trim().chars() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    if quoted || out.iter().any(String::is_empty) {
        return Err(format!(
            "`{key}` is not a setting name like `memory.enabled` or `prices.\"openai/gpt-5.5\"`"
        ));
    }
    Ok(out)
}

pub struct Settings;

impl Tool for Settings {
    fn name(&self) -> &'static str {
        "settings"
    }
    fn description(&self) -> String {
        "Tessera settings: `get`, or `set` a `key` to a JSON `value` (null resets), after user approval. \
         Chat helper limits: only when asked. Security, providers and credentials are the user's."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "set"]},
                "key": {"type": "string"},
                "value": {}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }
    fn concurrent(&self, args: &Value) -> bool {
        opt_str(args, "action") != Some("set")
    }
    fn summary(&self, args: &Value) -> String {
        // Shown whole when approving: the user must see exactly what is set.
        match (opt_str(args, "action"), opt_str(args, "key")) {
            (Some("set"), Some(k)) => {
                format!("{k} = {}", args.get("value").unwrap_or(&Value::Null))
            }
            _ => "show settings".into(),
        }
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if opt_str(&args, "action") != Some("set") {
                return match ctx.host.settings(&ctx.session_id) {
                    Ok(text) => ToolOutput::text(text),
                    Err(e) => ToolOutput::error(e),
                };
            }
            let Some(key) = opt_str(&args, "key") else {
                return ToolOutput::error("Missing `key`.");
            };
            let path = match parse_key(key) {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(e),
            };
            if let Some(why) = refusal(&path) {
                return ToolOutput::error(why);
            }
            if ctx.host.permission(&ctx.session_id) == PermissionMode::ReadOnly {
                return ToolOutput::error("This session is read-only; settings were not changed.");
            }
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            if value.to_string().len() > MAX_VALUE {
                return ToolOutput::error(
                    "That value is too long to approve here; ask the user to change it in config.toml.",
                );
            }
            // Always the user's decision, whatever the permission mode.
            let allowed = ctx
                .host
                .approve_reason(Approve {
                    session_id: ctx.session_id.clone(),
                    call_id: ctx.call_id.clone(),
                    tool: "settings".into(),
                    summary: self.summary(&args),
                    reason: "Changes Tessera's settings.".into(),
                    // "For the session" covers this exact change only.
                    key: format!("settings:{}={value}", path.join(".")),
                    always: true,
                    cancel: ctx.cancel.clone(),
                    arguments: serde_json::json!({}),
                    once: false,
                })
                .await;
            if let Err(why) = allowed {
                return ToolOutput::error(why);
            }
            match ctx.host.change_setting(&ctx.session_id, &path, &value) {
                Ok(text) => ToolOutput::text(text),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_and_only_everyday_settings_are_open() {
        assert_eq!(parse_key("memory.enabled").unwrap(), ["memory", "enabled"]);
        assert_eq!(
            parse_key("prices.\"openai/gpt-5.5\".input").unwrap(),
            ["prices", "openai/gpt-5.5", "input"]
        );
        assert!(parse_key("a..b").is_err() && parse_key("\"x").is_err());
        let ok = |k: &str| refusal(&parse_key(k).unwrap()).is_none();
        for k in [
            "memory.enabled",
            "models.fast",
            "default_model",
            "agent.max_steps",
            "lsp.enabled",
            "chat.helpers",
            "chat.helper_steps",
            "chat.helper_minutes",
        ] {
            assert!(ok(k), "{k}");
        }
        for k in [
            "security.sandbox",
            "security",
            "agent.permission",
            "agent.pass_env",
            "agent.pass_env.0",
            // The freedom new chats get on every device: the user's alone.
            "security.default_permission",
            "server.bind",
            // "Use Tessera in a web browser": the user's alone.
            "server.web_access",
            "provider",
            "mcp",
            "web.searxng_url",
            "browser.path",
            "skills.extra_dirs",
            "voice.base_url",
            "models",
            // The user's own terminals: never an agent's to turn on.
            "terminal.access",
            "terminal",
            "chat",
            "chat.permission",
        ] {
            assert!(!ok(k), "{k}");
        }
    }
}
