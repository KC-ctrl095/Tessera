//! `desktop`: see and use the screen of one of the user's machines that
//! has desktop control on (its signed-in Windows desktop). Each action goes
//! the machines way (`machines.rs`): the same choice of machine, its bot
//! access, asking first as that machine and this chat say, and the
//! machine's own checks and log. Offered only while a machine that can do
//! it has desktop control on (see `Inner::permitted_tool_specs`).

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tessera_proto::{DesktopOp, MouseButton, NodeAction, PermissionMode};

use super::{Risk, Tool, ToolCtx, ToolKind, ToolOutput, opt_bool, opt_str};

const ACTIONS: &[&str] = &[
    "screenshot",
    "click",
    "move",
    "drag",
    "scroll",
    "type",
    "key",
    "screens",
];

pub struct Desktop;

impl Tool for Desktop {
    fn name(&self) -> &'static str {
        "desktop"
    }
    fn description(&self) -> String {
        "See and use the screen of one of the user's machines with desktop control (its signed-in desktop). \
         Take a `screenshot` first: `x`/`y` are that picture's pixels (`screen` 1 is the main one). `click` \
         (`button`, `clicks: 2` double), `move`, `drag` to `to_x`/`to_y`, `scroll` (`dy` > 0 down), `type` text, \
         `key` (e.g. ctrl+c, alt+tab, enter), `screens`. `look: true` returns a screenshot after acting. The lock \
         screen, Windows security prompts and programs running as administrator are out of reach."
            .into()
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ACTIONS},
                "machine": {"type": "string", "description": "Name or id (optional when there is only one)"},
                "screen": {"type": "integer"},
                "x": {"type": "integer"},
                "y": {"type": "integer"},
                "to_x": {"type": "integer"},
                "to_y": {"type": "integer"},
                "button": {"type": "string", "enum": ["left", "right", "middle"]},
                "clicks": {"type": "integer"},
                "dx": {"type": "integer"},
                "dy": {"type": "integer"},
                "text": {"type": "string"},
                "keys": {"type": "string"},
                "look": {"type": "boolean"}
            },
            "required": ["action"]
        })
    }
    fn kind(&self) -> ToolKind {
        // Looking and acting in one tool: decided per action (read-only
        // chats may still look).
        ToolKind::Meta
    }
    fn concurrent(&self, args: &Value) -> bool {
        // Clicks and keys happen in the order they were asked for.
        matches!(opt_str(args, "action"), Some("screens" | "screenshot"))
    }
    fn summary(&self, args: &Value) -> String {
        let on = opt_str(args, "machine")
            .map(|m| format!("on {m}: "))
            .unwrap_or_default();
        match op_of(args) {
            Ok(op) => format!("{on}{}", op.describe()),
            Err(_) => format!("{on}{}", opt_str(args, "action").unwrap_or("screenshot")),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        if op_of(args).is_ok_and(|op| op.acts())
            && ctx.host.permission(&ctx.session_id) == PermissionMode::ReadOnly
        {
            return Risk::Block {
                reason: "This chat is read-only: it may look at a machine's screen, not use it."
                    .into(),
            };
        }
        Risk::None
    }
    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let op = match op_of(&args) {
                Ok(op) => op,
                Err(e) => return e,
            };
            let look = op.acts() && opt_bool(&args, "look").unwrap_or(false);
            let action = NodeAction::Desktop { desktop: op, look };
            match super::machines::act(&args, ctx, Some(action)).await {
                Ok(out) => out,
                Err(e) => e,
            }
        })
    }
}

fn int(args: &Value, key: &str) -> Option<i32> {
    args.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
        .and_then(|n| i32::try_from(n).ok())
}

fn need(args: &Value, key: &str) -> Result<i32, ToolOutput> {
    int(args, key).ok_or_else(|| {
        ToolOutput::error(format!(
            "Missing `{key}` (a whole number: pixels of the screenshot)."
        ))
    })
}

/// The desktop action `args` asks for.
fn op_of(args: &Value) -> Result<DesktopOp, ToolOutput> {
    let screen = int(args, "screen").and_then(|n| u32::try_from(n).ok());
    let action = opt_str(args, "action").unwrap_or("screenshot");
    Ok(match action {
        "screens" => DesktopOp::Screens,
        "screenshot" => DesktopOp::Screenshot { screen },
        "move" => DesktopOp::Move {
            screen,
            x: need(args, "x")?,
            y: need(args, "y")?,
        },
        "click" => DesktopOp::Click {
            screen,
            x: need(args, "x")?,
            y: need(args, "y")?,
            button: match opt_str(args, "button") {
                None | Some("left") => MouseButton::Left,
                Some("right") => MouseButton::Right,
                Some("middle") => MouseButton::Middle,
                Some(other) => {
                    return Err(ToolOutput::error(format!(
                        "Unknown button `{other}`; one of: left, right, middle."
                    )));
                }
            },
            clicks: int(args, "clicks").map(|c| c.clamp(1, 3) as u8),
        },
        "drag" => DesktopOp::Drag {
            screen,
            x: need(args, "x")?,
            y: need(args, "y")?,
            to_x: need(args, "to_x")?,
            to_y: need(args, "to_y")?,
        },
        "scroll" => {
            let (dx, dy) = (int(args, "dx").unwrap_or(0), int(args, "dy").unwrap_or(0));
            if dx == 0 && dy == 0 {
                return Err(ToolOutput::error(
                    "Say how far to scroll: `dy` (notches, > 0 down) or `dx` (> 0 right).",
                ));
            }
            DesktopOp::Scroll {
                screen,
                x: need(args, "x")?,
                y: need(args, "y")?,
                dx: dx.clamp(-50, 50),
                dy: dy.clamp(-50, 50),
            }
        }
        "type" => DesktopOp::Type {
            text: opt_str(args, "text")
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ToolOutput::error("Missing `text` to type."))?
                .to_string(),
        },
        "key" => DesktopOp::Key {
            keys: opt_str(args, "keys")
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .ok_or_else(|| ToolOutput::error("Missing `keys`, e.g. ctrl+c or enter."))?
                .to_string(),
        },
        other => {
            return Err(ToolOutput::error(format!(
                "Unknown action `{other}`; one of: {}.",
                ACTIONS.join(", ")
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_become_desktop_actions() {
        let op = op_of(&json!({"action": "click", "x": 10, "y": 20.4, "clicks": 2})).unwrap();
        assert_eq!(
            op,
            DesktopOp::Click {
                screen: None,
                x: 10,
                y: 20,
                button: MouseButton::Left,
                clicks: Some(2)
            }
        );
        assert_eq!(op.describe(), "double-click 10,20");
        assert!(op.acts());
        let shot = op_of(&json!({"action": "screenshot", "screen": 2})).unwrap();
        assert_eq!(shot.describe(), "screenshot on screen 2");
        assert!(!shot.acts());
        // Typed text isn't repeated where it is described (logs, approvals).
        let t = op_of(&json!({"action": "type", "text": "hello there"})).unwrap();
        assert_eq!(t.describe(), "type 11 characters");
        assert!(op_of(&json!({"action": "click", "x": 1})).is_err());
        assert!(op_of(&json!({"action": "scroll", "x": 1, "y": 1})).is_err());
        assert!(op_of(&json!({"action": "click", "x": 1, "y": 1, "button": "side"})).is_err());
        assert!(op_of(&json!({"action": "wave"})).is_err());
    }
}
