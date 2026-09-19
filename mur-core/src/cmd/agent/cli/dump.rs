//! Render the visible transcript to plain text for the Ctrl+O scrollback dump.
//! Tool cards are expanded past the TUI's line caps (upstream byte-truncated
//! output can't be recovered) and unstyled, so the user can
//! select/copy/search it natively.

use super::app::{ChatMsg, Role};
use super::step::StepCard;

/// The whole visible transcript as plain, unstyled text — tool cards expanded
/// past the TUI's line caps — for the Ctrl+O scrollback dump.
pub fn transcript_to_text(messages: &[ChatMsg]) -> String {
    let mut out = String::new();
    for m in messages {
        if let Some(card) = &m.step {
            out.push_str(&card_text(card));
            continue;
        }
        match m.role {
            Role::User => {
                out.push_str("\nyou> ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::Agent => {
                out.push('\n');
                if !m.thinking.is_empty() {
                    out.push_str("[reasoning]\n");
                    out.push_str(&m.thinking);
                    out.push('\n');
                }
                out.push_str("agent> ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::System => {
                out.push_str("· ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::Shell => {
                // already formatted as "$ cmd\noutput"
                out.push_str(&m.text);
                out.push('\n');
            }
        }
    }
    out
}

fn card_text(card: &StepCard) -> String {
    let mut s = String::new();
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    s.push_str(&format!("\n{} {}{}\n", card.glyph(), card.name, dur));
    // An edit tool's args are a file path plus two big blobs of source. Dumped
    // as JSON they arrive as single `\n`-escaped lines — the least readable
    // form of the thing the card exists to show. The TUI card already renders
    // these as a diff; the dump gets the same rows, uncapped (the dump exists
    // precisely to escape the TUI's line caps).
    if let Some(diff_lines) = super::diff::edit_diff_text(&card.name, &card.args) {
        for l in diff_lines {
            s.push_str("  ");
            s.push_str(&l);
            s.push('\n');
        }
    } else if !card.args.is_null()
        && let Ok(pretty) = serde_json::to_string_pretty(&card.args)
    {
        for l in pretty.lines() {
            s.push_str("  ");
            s.push_str(l);
            s.push('\n');
        }
    }
    if let Some(err) = &card.error {
        s.push_str(&format!("  ✗ {err}\n"));
    }
    if !card.output.is_empty() {
        for l in card.output.lines() {
            s.push_str("  ");
            s.push_str(l);
            s.push('\n');
        }
        if card.truncated {
            s.push_str(&format!(
                "  … (output truncated to {} bytes; {} total)\n",
                card.output.len(),
                card.full_len
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::transcript_to_text;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::step::{CallOutcome, StepCard};

    #[test]
    fn renders_user_and_agent_and_reasoning() {
        let msgs = vec![ChatMsg::for_test(Role::User, "hello"), {
            let mut m = ChatMsg::for_test(Role::Agent, "hi there");
            m.thinking = "let me think".into();
            m
        }];
        let t = transcript_to_text(&msgs);
        assert!(t.contains("you> hello"));
        assert!(t.contains("let me think")); // reasoning kept in the dump
        assert!(t.contains("agent> hi there"));
    }

    #[test]
    fn renders_tool_card_fully_expanded() {
        let mut card = StepCard::new(
            "s1".into(),
            "bash".into(),
            serde_json::json!({"command":"ls"}),
        );
        card.complete(CallOutcome::Ok, "a.rs\nb.rs".into(), false, 2, None, 5);
        let m = ChatMsg::tool_for_test(card);
        let t = transcript_to_text(&[m]);
        assert!(t.contains("bash"));
        assert!(t.contains("\"command\": \"ls\"")); // full args
        assert!(t.contains("a.rs")); // full output
        assert!(t.contains("b.rs"));
    }

    #[test]
    fn edit_card_dumps_a_diff_not_escaped_json() {
        // `new_string` as JSON is one `\n`-escaped line; the dump must show
        // the same +/- rows the TUI card does.
        let mut card = StepCard::new(
            "s1".into(),
            "edit_file".into(),
            serde_json::json!({
                "path": "src/lib.rs",
                "old_string": "let x = 1;\nlet y = 2;",
                "new_string": "let x = 9;\nlet y = 2;",
            }),
        );
        card.complete(CallOutcome::Ok, "ok".into(), false, 1, None, 2);
        let t = transcript_to_text(&[ChatMsg::tool_for_test(card)]);
        assert!(t.contains("- let x = 1;"), "expected removal, got:\n{t}");
        assert!(t.contains("+ let x = 9;"), "expected addition, got:\n{t}");
        assert!(t.contains("src/lib.rs"), "expected path header, got:\n{t}");
        assert!(
            !t.contains("\\n"),
            "dump must not contain escaped newlines, got:\n{t}"
        );
        assert!(
            !t.contains("\"new_string\""),
            "raw JSON args must not survive, got:\n{t}"
        );
    }

    #[test]
    fn edit_card_dump_is_uncapped() {
        // The TUI card caps diffs at 40 lines; the dump exists to escape caps,
        // so all 60 additions must be present and no "+N more" marker.
        let content = (0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut card = StepCard::new(
            "s1".into(),
            "write_file".into(),
            serde_json::json!({ "path": "big.rs", "content": content }),
        );
        card.complete(CallOutcome::Ok, "ok".into(), false, 1, None, 2);
        let t = transcript_to_text(&[ChatMsg::tool_for_test(card)]);
        assert!(t.contains("+ line 0"), "missing first line:\n{t}");
        assert!(t.contains("+ line 59"), "missing 60th line:\n{t}");
        assert!(
            !t.contains("more diff line(s)"),
            "dump must not truncate, got:\n{t}"
        );
    }

    #[test]
    fn non_edit_card_still_dumps_json_args() {
        let mut card = StepCard::new(
            "s1".into(),
            "bash".into(),
            serde_json::json!({"command":"ls"}),
        );
        card.complete(CallOutcome::Ok, "a.rs".into(), false, 1, None, 2);
        let t = transcript_to_text(&[ChatMsg::tool_for_test(card)]);
        assert!(t.contains("\"command\": \"ls\""), "got:\n{t}");
    }

    #[test]
    fn renders_error_card() {
        let mut card = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        card.complete(
            CallOutcome::Failed,
            "boom".into(),
            false,
            4,
            Some("exit 1".into()),
            3,
        );
        let t = transcript_to_text(&[ChatMsg::tool_for_test(card)]);
        assert!(t.contains("exit 1"));
    }
}
