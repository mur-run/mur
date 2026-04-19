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
    let mut pending_role: Option<Role> = None;
    let mut buf = String::new();
    for raw in md.lines() {
        let line = raw.trim_start();
        if let Some(user_text) = line.strip_prefix("#### > ") {
            // Flush anything accumulated (typically the prior assistant response)
            flush(&mut out, &mut pending_role, &mut buf, chat_id);
            // Emit user turn immediately — it's a single-line prompt
            let trimmed = user_text.trim();
            if !trimmed.is_empty() {
                out.push(Message {
                    v: 1,
                    ts: Utc::now(),
                    src: Source::Aider,
                    conv: chat_id.into(),
                    role: Role::User,
                    content: Content::Text {
                        value: trimmed.into(),
                    },
                    meta: serde_json::Value::Null,
                    refs: vec![],
                });
            }
            // Next non-`####` content belongs to the assistant
            pending_role = Some(Role::Assistant);
        } else if line.starts_with("####") {
            flush(&mut out, &mut pending_role, &mut buf, chat_id);
        } else if pending_role.is_some() {
            buf.push_str(raw);
            buf.push('\n');
        }
    }
    flush(&mut out, &mut pending_role, &mut buf, chat_id);
    Ok(out)
}

fn flush(out: &mut Vec<Message>, role: &mut Option<Role>, buf: &mut String, chat_id: &str) {
    if let Some(r) = role.take()
        && !buf.trim().is_empty()
    {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_history_with_role_separation() {
        let md = r"# aider chat started at 2026-04-19

#### test 1

#### > hello aider

hi back

#### > bye

bye
";
        let msgs = parse_aider_md(md, "chat-1").unwrap();
        assert_eq!(msgs.len(), 4, "expected user/asst/user/asst, got {msgs:?}");
        assert!(matches!(msgs[0].role, Role::User));
        assert!(matches!(msgs[1].role, Role::Assistant));
        assert!(matches!(msgs[2].role, Role::User));
        assert!(matches!(msgs[3].role, Role::Assistant));
        if let Content::Text { value } = &msgs[0].content {
            assert_eq!(value, "hello aider");
        } else {
            panic!("expected text");
        }
        if let Content::Text { value } = &msgs[1].content {
            assert_eq!(value, "hi back");
        } else {
            panic!("expected text");
        }
        if let Content::Text { value } = &msgs[2].content {
            assert_eq!(value, "bye");
        } else {
            panic!("expected text");
        }
        if let Content::Text { value } = &msgs[3].content {
            assert_eq!(value, "bye");
        } else {
            panic!("expected text");
        }
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(parse_aider_md("", "c").unwrap().is_empty());
    }

    #[test]
    fn user_prompt_without_assistant_reply_emits_only_user() {
        let md = "#### > solo question\n";
        let msgs = parse_aider_md(md, "c").unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0].role, Role::User));
    }

    #[test]
    fn multi_line_assistant_response_preserved() {
        let md = "#### > prompt\n\nline one\nline two\nline three\n";
        let msgs = parse_aider_md(md, "c").unwrap();
        assert_eq!(msgs.len(), 2);
        if let Content::Text { value } = &msgs[1].content {
            assert_eq!(value, "line one\nline two\nline three");
        } else {
            panic!("expected text");
        }
    }
}
