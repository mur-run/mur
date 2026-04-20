//! Stage 1: tool-call pointer substitution (spec §4.3).
use anyhow::Result;
use mur_common::{Content, Message, Role};

use super::super::blob;

pub const NORM_THRESHOLD: usize = 10 * 1024;

pub fn normalize(msg: Message, root_override: Option<&str>) -> Result<Message> {
    if !matches!(msg.role, Role::Tool) {
        return Ok(msg);
    }
    let Content::Text { value } = &msg.content else {
        return Ok(msg);
    };
    if value.len() < NORM_THRESHOLD {
        return Ok(msg);
    }
    let bytes = value.len() as u64;
    let sha256 = blob::put(value.as_bytes(), root_override)?;
    let path = msg
        .meta
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("<in-blob>")
        .to_string();
    let desc = msg
        .meta
        .get("tool")
        .and_then(|v| v.as_str())
        .map(|t| format!("{} result", t))
        .unwrap_or_else(|| "tool result".into());
    let mut out = msg;
    out.content = Content::ToolRef {
        sha256,
        path,
        bytes,
        desc,
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn tool_msg(text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc::now(),
            src: Source::ClaudeCode,
            conv: "c".into(),
            role: Role::Tool,
            content: Content::Text { value: text.into() },
            meta: serde_json::json!({"tool": "Read", "path": "src/main.rs"}),
            refs: vec![],
        }
    }

    #[test]
    fn small_text_passes_through() {
        let tmp = tempfile::tempdir().unwrap();
        let out = normalize(tool_msg("short"), Some(tmp.path().to_str().unwrap())).unwrap();
        assert!(matches!(out.content, Content::Text { .. }));
    }

    #[test]
    fn large_text_becomes_tool_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(11_000);
        let out = normalize(tool_msg(&big), Some(tmp.path().to_str().unwrap())).unwrap();
        match out.content {
            Content::ToolRef { sha256, bytes, .. } => {
                assert_eq!(bytes, 11_000);
                assert_eq!(sha256.len(), 64);
            }
            _ => panic!("expected ToolRef"),
        }
    }

    #[test]
    fn user_role_never_pointerized() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = tool_msg(&"x".repeat(11_000));
        m.role = Role::User;
        let out = normalize(m, Some(tmp.path().to_str().unwrap())).unwrap();
        assert!(matches!(out.content, Content::Text { .. }));
    }
}
