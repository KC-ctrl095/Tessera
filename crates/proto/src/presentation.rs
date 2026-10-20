//! Display-only tool metadata. Execution, approval and audit always use the
//! original name and arguments. Apps and the terminal share the alias table.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
struct Alias {
    kind: String,
    #[serde(default)]
    housekeeping: bool,
    #[serde(default)]
    helper: bool,
    action: Option<String>,
}

fn aliases() -> &'static HashMap<String, Alias> {
    static ALIASES: OnceLock<HashMap<String, Alias>> = OnceLock::new();
    ALIASES.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../apps/tessera/ui/src/lib/tool-presentation.json"
        ))
        .expect("tool presentation aliases")
    })
}

/// A canonical display kind/input, never a replacement executable call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPresentation {
    pub kind: String,
    pub arguments: Value,
    pub housekeeping: bool,
    pub helper: Option<&'static str>,
}

impl ToolPresentation {
    pub fn for_call(name: &str, args: &Value) -> Self {
        let name = name.strip_prefix("mcp__tessera__").unwrap_or(name);
        let alias = aliases().get(name);
        let kind =
            if name == "agents" && args.get("action").and_then(Value::as_str) == Some("start") {
                "agent"
            } else {
                alias.map(|a| a.kind.as_str()).unwrap_or(name)
            };
        let mut arguments = args.clone();
        if let Some(o) = arguments.as_object_mut() {
            if name == "agents" && kind == "agent" {
                o.remove("action");
            }
            for (key, variants) in [
                ("path", &["file_path", "filePath"][..]),
                ("command", &["cmd", "script"][..]),
                ("background", &["run_in_background"][..]),
                ("id", &["task_id", "shell_id", "taskId"][..]),
            ] {
                // An external tool's keys have no native meaning.
                if !matches!(
                    kind,
                    "read" | "write" | "edit" | "bash" | "job" | "grep" | "glob" | "task"
                ) {
                    continue;
                }
                if !o.contains_key(key)
                    && let Some(v) = variants.iter().find_map(|k| o.get(*k)).cloned()
                {
                    o.insert(key.into(), v);
                }
                for v in variants {
                    o.remove(*v);
                }
            }
            if let Some(action) = alias.and_then(|a| a.action.as_ref()) {
                o.insert("action".into(), json!(action));
            }
            if alias.is_some_and(|a| a.helper) && !o.contains_key("tasks") {
                let title = o
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("Helper");
                arguments = json!({"tasks":[{"name":title,"prompt":o.get("prompt").cloned().unwrap_or(Value::Null)}],"wait":o.get("run_in_background").and_then(Value::as_bool)!=Some(true)});
            }
        }
        Self {
            kind: kind.into(),
            arguments,
            housekeeping: alias.is_some_and(|a| a.housekeeping),
            helper: (kind == "agent").then_some(
                if alias.is_some_and(|a| a.helper)
                    && args.get("tasks").and_then(Value::as_array).is_none()
                {
                    "read_only"
                } else {
                    "controls"
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_presentation_keeps_execution_identity_and_unknown_tools() {
        let raw = json!({"file_path":"src/main.rs","old_string":"old","new_string":"new"});
        let p = ToolPresentation::for_call("Edit", &raw);
        assert_eq!(p.kind, "edit");
        assert_eq!(p.arguments["path"], "src/main.rs");
        assert!(raw.get("path").is_none());
        assert_eq!(
            ToolPresentation::for_call("NotebookEdit", &raw).arguments,
            raw
        );
        assert_eq!(
            ToolPresentation::for_call("mcp__lab__Read", &raw).kind,
            "mcp__lab__Read"
        );
        assert_eq!(ToolPresentation::for_call("Task", &json!({})).kind, "agent");
        assert!(ToolPresentation::for_call("task", &json!({})).housekeeping);
    }
}
