//! Claude Code ingester. Existing hooks (on-prompt/on-tool/on-stop) call
//! `mur session record`, which — when conversations is enabled — also
//! pushes through the pre-filter pipeline.
//!
//! Event-type mapping:
//!   "user"        -> Role::User
//!   "assistant"   -> Role::Assistant
//!   "tool_call"   -> Role::Tool
//!   "tool_result" -> Role::Tool
//!   "system"      -> Role::System

use anyhow::{Result, bail};
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};

pub fn event_to_message(
    event_type: &str,
    tool: Option<&str>,
    content: &str,
    session_id: &str,
) -> Result<Message> {
    let role = match event_type {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool_call" | "tool" | "tool_result" => Role::Tool,
        "system" => Role::System,
        other => bail!("unknown event type: {other}"),
    };
    let mut meta = serde_json::json!({});
    if let Some(t) = tool {
        meta["tool"] = serde_json::Value::String(t.into());
    }
    Ok(Message {
        v: 1,
        ts: Utc::now(),
        src: Source::ClaudeCode,
        conv: session_id.to_string(),
        role,
        content: Content::Text {
            value: content.to_string(),
        },
        meta,
        refs: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::Role;

    #[test]
    fn event_to_message_user() {
        let m = event_to_message("user", None, "hi there", "sess-123").unwrap();
        assert!(matches!(m.role, Role::User));
        assert_eq!(m.conv, "sess-123");
    }

    #[test]
    fn event_to_message_tool_with_meta() {
        let m = event_to_message("tool_call", Some("Read"), "{\"path\":\"x\"}", "sess").unwrap();
        assert!(matches!(m.role, Role::Tool));
        assert_eq!(m.meta.get("tool").and_then(|v| v.as_str()), Some("Read"));
    }

    #[test]
    fn unknown_event_type_errors() {
        assert!(event_to_message("banana", None, "x", "s").is_err());
    }
}
