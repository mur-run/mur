//! Cursor ingester. Two inputs:
//! 1. SQLite at ~/Library/Application Support/Cursor/User/workspaceStorage/*/state.vscdb
//! 2. .specstory/ markdown files (public export format)
//!
//! Schema is unstable; best-effort with skip-on-failure.
#![allow(dead_code)] // Phase 1: scan_workspace wired in by later tasks (CLI).

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::{Path, PathBuf};

pub fn list_cursor_workspaces() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let base = home.join("Library/Application Support/Cursor/User/workspaceStorage");
    if !base.exists() {
        return Vec::new();
    }
    match std::fs::read_dir(&base) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn scan_workspace(ws_dir: &Path) -> Result<Vec<Message>> {
    let db = ws_dir.join("state.vscdb");
    if db.exists()
        && let Ok(msgs) = scan_vscdb(&db, ws_dir)
    {
        return Ok(msgs);
    }
    Ok(Vec::new())
}

fn scan_vscdb(db: &Path, ws_dir: &Path) -> Result<Vec<Message>> {
    let conn = rusqlite::Connection::open(db).with_context(|| format!("open {db:?}"))?;
    let ws = ws_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("ws")
        .to_string();

    let mut stmt =
        conn.prepare("SELECT value FROM ItemTable WHERE key LIKE '%aiChat%' OR key LIKE '%chat%'")?;
    let rows = stmt.query_map([], |row| {
        let text: String = row.get(0)?;
        Ok(text)
    })?;
    let mut out = Vec::new();
    for row in rows {
        let text = row?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Ok(mut msgs) = parse_cursor_chat_blob(&v, &ws) {
            out.append(&mut msgs);
        }
    }
    Ok(out)
}

pub fn parse_cursor_chat_blob(v: &serde_json::Value, workspace_hash: &str) -> Result<Vec<Message>> {
    let chats = v.get("chats").and_then(|c| c.as_array());
    let Some(chats) = chats else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for chat in chats {
        let id = chat.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let Some(msgs) = chat.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for m in msgs {
            let role = match m.get("role").and_then(|v| v.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                Some("tool") => Role::Tool,
                _ => continue,
            };
            let Some(content) = m.get("content").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push(Message {
                v: 1,
                ts: Utc::now(),
                src: Source::Cursor,
                conv: format!("{workspace_hash}/{id}"),
                role,
                content: Content::Text {
                    value: content.to_string(),
                },
                meta: serde_json::json!({"workspace": workspace_hash}),
                refs: vec![],
            });
        }
    }
    Ok(out)
}

pub fn parse_specstory_md(md: &str, chat_id: &str) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut current_role: Option<Role> = None;
    let mut current_buf = String::new();
    for raw in md.lines() {
        let line = raw.trim();
        if let Some(stripped) = line.strip_prefix("#### ") {
            flush(&mut out, &mut current_role, &mut current_buf, chat_id);
            current_role = match stripped.to_lowercase().as_str() {
                "user" => Some(Role::User),
                "assistant" => Some(Role::Assistant),
                _ => None,
            };
        } else if line == "---" {
            flush(&mut out, &mut current_role, &mut current_buf, chat_id);
        } else if current_role.is_some() {
            current_buf.push_str(raw);
            current_buf.push('\n');
        }
    }
    flush(&mut out, &mut current_role, &mut current_buf, chat_id);
    Ok(out)
}

fn flush(out: &mut Vec<Message>, current_role: &mut Option<Role>, buf: &mut String, chat_id: &str) {
    if let Some(role) = current_role.take()
        && !buf.trim().is_empty()
    {
        out.push(Message {
            v: 1,
            ts: Utc::now(),
            src: Source::Cursor,
            conv: chat_id.into(),
            role,
            content: Content::Text {
                value: buf.trim().into(),
            },
            meta: serde_json::Value::Null,
            refs: vec![],
        });
    }
    buf.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_chat_from_json_blob() {
        let blob = serde_json::json!({
            "chats": [{
                "id": "chat-1",
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "hi back"},
                ]
            }]
        });
        let msgs = parse_cursor_chat_blob(&blob, "workspace-x").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].conv, "workspace-x/chat-1");
    }

    #[test]
    fn unknown_schema_returns_empty_not_error() {
        let blob = serde_json::json!({"unexpected": "shape"});
        let msgs = parse_cursor_chat_blob(&blob, "ws").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn specstory_md_parser_extracts_turns() {
        let md = "#### User\n\nhello\n\n---\n\n#### Assistant\n\nhi\n";
        let msgs = parse_specstory_md(md, "my-chat").unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
