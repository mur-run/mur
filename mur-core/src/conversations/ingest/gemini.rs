//! Gemini CLI ingester. Reads ~/.gemini/tmp/<hash>/chats/*.json.
//!
//! Schema: `{ history: [ { role, parts: [ {text} ] } ] }`. Gemini uses "model"
//! for assistant; we map to `Role::Assistant`.
#![allow(dead_code)] // Phase 1: list_gemini_chats wired in by later tasks (CLI).

use anyhow::Result;
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::PathBuf;

pub fn list_gemini_chats() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let base = home.join(".gemini").join("tmp");
    if !base.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&base) else {
        return out;
    };
    for hash_dir in rd.flatten() {
        let chats = hash_dir.path().join("chats");
        if !chats.exists() {
            continue;
        }
        let Ok(rd2) = std::fs::read_dir(&chats) else {
            continue;
        };
        for f in rd2.flatten() {
            if f.path().extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(f.path());
            }
        }
    }
    out
}

pub fn parse_gemini_chat(v: &serde_json::Value, session_id: &str) -> Result<Vec<Message>> {
    let Some(history) = v.get("history").and_then(|h| h.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for turn in history {
        let role = match turn.get("role").and_then(|v| v.as_str()) {
            Some("user") => Role::User,
            Some("model") | Some("assistant") => Role::Assistant,
            Some("system") => Role::System,
            _ => continue,
        };
        let mut text = String::new();
        if let Some(parts) = turn.get("parts").and_then(|p| p.as_array()) {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                }
            }
        }
        if text.is_empty() {
            continue;
        }
        out.push(Message {
            v: 1,
            ts: Utc::now(),
            src: Source::Gemini,
            conv: session_id.into(),
            role,
            content: Content::Text { value: text },
            meta: serde_json::Value::Null,
            refs: vec![],
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gemini_chat_json() {
        let j = serde_json::json!({
            "history": [
                {"role": "user", "parts": [{"text": "hello"}]},
                {"role": "model", "parts": [{"text": "hi"}]}
            ]
        });
        let msgs = parse_gemini_chat(&j, "sess-1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].conv, "sess-1");
        assert!(matches!(msgs[1].role, mur_common::Role::Assistant));
    }

    #[test]
    fn multi_part_concatenates() {
        let j = serde_json::json!({
            "history": [
                {"role": "user", "parts": [{"text": "hello "}, {"text": "world"}]}
            ]
        });
        let msgs = parse_gemini_chat(&j, "s").unwrap();
        assert_eq!(msgs.len(), 1);
        if let mur_common::Content::Text { value } = &msgs[0].content {
            assert_eq!(value, "hello world");
        } else {
            panic!("expected text");
        }
    }
}
