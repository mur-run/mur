//! Aider ingester. Scans configured `watched_dirs` for `.aider.chat.history.md`.
//!
//! User turns: lines prefixed with `#### > `. Assistant turns: everything else
//! between separators. Unknown content before the first marker is ignored.
#![allow(dead_code)] // Phase 1: find_aider_histories wired in by later tasks (CLI).

use anyhow::Result;
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::PathBuf;

pub fn find_aider_histories(watched: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in watched {
        walk(root, &mut out);
    }
    out
}

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            let n = e.file_name();
            let name = n.to_string_lossy();
            if matches!(name.as_ref(), "node_modules" | "target" | ".git") {
                continue;
            }
            walk(&p, out);
        } else if p.file_name().and_then(|s| s.to_str()) == Some(".aider.chat.history.md") {
            out.push(p);
        }
    }
}

pub fn parse_aider_md(md: &str, chat_id: &str) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut current_role: Option<Role> = None;
    let mut buf = String::new();
    for raw in md.lines() {
        let line = raw.trim_start();
        if let Some(user_text) = line.strip_prefix("#### > ") {
            flush(&mut out, &mut current_role, &mut buf, chat_id);
            current_role = Some(Role::User);
            buf.push_str(user_text);
            buf.push('\n');
        } else if line.starts_with("####") {
            flush(&mut out, &mut current_role, &mut buf, chat_id);
        } else if current_role.is_none() && !line.is_empty() {
            current_role = Some(Role::Assistant);
            buf.push_str(raw);
            buf.push('\n');
        } else if current_role.is_some() {
            buf.push_str(raw);
            buf.push('\n');
        }
    }
    flush(&mut out, &mut current_role, &mut buf, chat_id);
    Ok(out)
}

fn flush(out: &mut Vec<Message>, role: &mut Option<Role>, buf: &mut String, chat_id: &str) {
    if let Some(r) = role.take() {
        if !buf.trim().is_empty() {
            out.push(Message {
                v: 1,
                ts: Utc::now(),
                src: Source::Aider,
                conv: chat_id.into(),
                role: r,
                content: Content::Text {
                    value: buf.trim().into(),
                },
                meta: serde_json::Value::Null,
                refs: vec![],
            });
        }
        buf.clear();
    } else {
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_history() {
        let md = r"# aider chat started at 2026-04-19

#### test 1

#### > hello aider

hi back

#### > bye

bye
";
        let msgs = parse_aider_md(md, "chat-1").unwrap();
        assert!(msgs.len() >= 2);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(parse_aider_md("", "c").unwrap().is_empty());
    }
}
