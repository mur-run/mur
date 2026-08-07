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
    if !card.args.is_null()
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
