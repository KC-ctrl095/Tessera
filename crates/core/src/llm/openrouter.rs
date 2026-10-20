//! OpenRouter's additions to the Chat Completions body: prompt caching for
//! the models that need markers, and sticky routing for a conversation.
//!
//! OpenAI, DeepSeek, Grok and Gemini 2.5+ cache prompts by themselves.
//! Claude caches only where the request marks it: without markers every
//! step of a tool loop pays full price for the whole history (Anthropic's
//! own API gets the same markers in `anthropic.rs`).

use serde_json::{Value, json};

/// Whether `base_url` is OpenRouter.
pub fn is_openrouter(base_url: &str) -> bool {
    base_url.contains("openrouter.ai")
}

/// Claude models (`anthropic/claude-…`) cache only with explicit markers.
fn needs_markers(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("anthropic/") || m.starts_with("~anthropic/") || m.contains("claude")
}

/// Add caching to a request body built by `openai_chat::build_body`.
///
/// For Claude: a breakpoint on the system prompt (which, with the tools
/// before it, never changes in a session) and the automatic one that
/// follows the end of the conversation (a tool message can't carry a
/// marker of its own; OpenRouter places it on every Claude host). The
/// `session_id` (`cache_key`, the chat's id) keeps the conversation on the
/// provider that holds its cache, for every model.
pub fn add_caching(body: &mut Value, model: &str, session: &str) {
    if !session.is_empty() && session.len() <= 256 {
        body["session_id"] = json!(session);
    }
    if !needs_markers(model) {
        return;
    }
    if let Some(sys) = body
        .get_mut("messages")
        .and_then(|m| m.get_mut(0))
        .filter(|m| m["role"] == "system")
        && let Some(text) = sys["content"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    {
        sys["content"] =
            json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
    }
    body["cache_control"] = json!({"type": "ephemeral"});
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "data"}
            ],
            "stream": true
        })
    }

    #[test]
    fn claude_gets_a_system_breakpoint_and_the_automatic_one() {
        let mut b = body();
        add_caching(&mut b, "anthropic/claude-sonnet-5.5", "sess-1");
        assert_eq!(
            b["messages"][0]["content"],
            json!([{"type": "text", "text": "You are helpful.", "cache_control": {"type": "ephemeral"}}])
        );
        assert_eq!(b["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(b["session_id"], "sess-1");
        // The conversation itself is untouched (a tool message stays a string).
        assert_eq!(b["messages"][3]["content"], "data");
        assert_eq!(b["messages"][1]["content"], "hi");
    }

    #[test]
    fn models_that_cache_by_themselves_only_get_the_session() {
        for model in [
            "openai/gpt-6.1",
            "google/gemini-3-pro",
            "deepseek/deepseek-v3.2",
        ] {
            let mut b = body();
            add_caching(&mut b, model, "sess-1");
            assert_eq!(b["messages"][0]["content"], "You are helpful.", "{model}");
            assert!(b.get("cache_control").is_none(), "{model}");
            assert_eq!(b["session_id"], "sess-1");
        }
        // No usable session id: nothing added.
        let mut b = body();
        add_caching(&mut b, "openai/gpt-6.1", "");
        assert!(b.get("session_id").is_none());
    }

    #[test]
    fn usage_counts_cache_reads_and_writes_apart() {
        let u = super::super::openai_chat::parse_usage(&json!({
            "prompt_tokens": 10_000, "completion_tokens": 60,
            "prompt_tokens_details": {"cached_tokens": 8_000, "cache_write_tokens": 1_500}
        }));
        assert_eq!(
            (u.input, u.cache_read, u.cache_write, u.output),
            (500, 8_000, 1_500, 60)
        );
    }

    #[test]
    fn an_empty_system_prompt_gets_no_marker() {
        let mut b = body();
        b["messages"][0]["content"] = json!("");
        add_caching(&mut b, "anthropic/claude-opus-5.5", "s");
        assert_eq!(b["messages"][0]["content"], "");
        assert_eq!(b["cache_control"], json!({"type": "ephemeral"}));
    }
}
