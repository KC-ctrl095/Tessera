//! `notify`: a notification on the user's phone and computers, now or at
//! a set time. Recurring ones are scheduled jobs that call it.

use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolKind, ToolOutput};

pub struct Notify;

/// What the host needs to send one.
pub struct Outgoing {
    pub title: String,
    pub body: String,
    pub urgent: bool,
    /// Unix ms; `None` = now.
    pub at: Option<u64>,
}

/// What became of it.
pub struct Sent {
    pub id: u64,
    pub due_at: u64,
    pub scheduled: bool,
    pub quiet: bool,
    /// The devices that take notifications.
    pub devices: Vec<String>,
}

impl Tool for Notify {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn description(&self) -> String {
        "Notify the user on their devices, even with Tessera closed. Only requested news or awaited results, not progress. Scheduled checks notify only for a meaningful change or needed action, never every run. `at` sends later.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "body": {"type": "string", "description": "Plain text"},
                "at": {"type": "string", "description": "e.g. `in 20m`, `15:30`, `tomorrow 9:00`"},
                "urgent": {"type": "boolean"},
                "cancel": {"type": "integer", "description": "Scheduled id to cancel"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    fn summary(&self, args: &Value) -> String {
        match args["cancel"].as_u64() {
            Some(id) => format!("cancel #{id}"),
            None => args["title"]
                .as_str()
                .or(args["body"].as_str())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect(),
        }
    }

    fn run<'a>(&'a self, args: Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Some(id) = args["cancel"].as_u64() {
                return match ctx.host.cancel_notification(&ctx.session_id, id) {
                    Ok(()) => ToolOutput::text(format!("Cancelled notification {id}.")),
                    Err(e) => ToolOutput::error(e),
                };
            }
            let text = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
            let (title, body) = (text("title"), text("body"));
            if title.is_empty() && body.is_empty() {
                return ToolOutput::error("Give a `title` and a `body`.");
            }
            let at = match args["at"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                None => None,
                Some(s) => match crate::jobs::parse_when(s, crate::util::now_ms()) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        return ToolOutput::error(format!(
                            "{e}. Use e.g. `in 20m`, `15:30`, `tomorrow 9:00` or `2026-10-02 14:00`."
                        ));
                    }
                },
            };
            let out = Outgoing {
                title,
                body,
                urgent: args["urgent"].as_bool().unwrap_or(false),
                at,
            };
            match ctx.host.notify(&ctx.session_id, out) {
                Ok(sent) if sent.scheduled => ToolOutput::text(format!(
                    "Scheduled for {} (server time), id {}. It will show on: {}.",
                    crate::jobs::fmt_time(sent.due_at),
                    sent.id,
                    list(&sent.devices)
                ))
                .with_details(json!({"notification": sent.id, "due_at": sent.due_at})),
                Ok(sent) if sent.quiet => ToolOutput::text("Saved quietly in the inbox. Already seen, repeated or too frequent to alert again.")
                    .with_details(json!({"notification": sent.id, "quiet": true})),
                Ok(sent) => ToolOutput::text(format!("Notification saved for receiving devices: {}.", list(&sent.devices)))
                    .with_details(json!({"notification": sent.id})),
                Err(e) => ToolOutput::error(e),
            }
        })
    }
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "no device yet".into()
    } else {
        names.join(", ")
    }
}
