//! Pure logic for agent-suggested replies: extract the `replies` array from a
//! `suggest_replies` tool call, and decide how to reveal them. No TUI, no I/O.

/// Hard cap on suggestions shown (matches the tool schema's maxItems).
pub const MAX_SUGGESTIONS: usize = 5;

/// The runtime tool name the TUI intercepts (mirrors mur-agent-runtime's
/// `tools::suggest::SUGGEST_REPLIES`). Kept in sync by spec; both are the
/// literal string "suggest_replies".
pub const SUGGEST_REPLIES_NAME: &str = "suggest_replies";

/// One quick-reply option: the message the user would send, plus an optional
/// one-line description of the trade-off (Claude-Code-style option list).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The message sent verbatim if the user picks this option.
    pub text: String,
    /// Optional one-line rationale shown dimmed under the option.
    pub desc: Option<String>,
}

/// Extract quick-reply options from the tool-call args, capped. Each item may be
/// a bare string (legacy) or `{text, description?}`. Fail-soft: any malformed
/// shape yields an empty vec, and malformed individual items are skipped.
pub fn parse_suggestions(args: &serde_json::Value) -> Vec<Suggestion> {
    let Some(arr) = args.get("replies").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let (text, desc) = if let Some(s) = v.as_str() {
                (s, None)
            } else {
                let text = v.get("text").and_then(|t| t.as_str())?;
                let desc = v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string);
                (text, desc)
            };
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            Some(Suggestion {
                text: text.to_string(),
                desc,
            })
        })
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// How to surface a set of pending suggestions.
#[derive(Debug, Clone, PartialEq)]
pub enum Reveal {
    /// Nothing to show (empty, or the composer already has text).
    None,
    /// A single suggestion → ghost placeholder text (the `text`, no description).
    Ghost(String),
    /// Two or more → a chooser overlay.
    Chooser(Vec<Suggestion>),
}

/// Decide how to reveal `pending` given whether the composer is empty.
pub fn plan_reveal(pending: Vec<Suggestion>, input_empty: bool) -> Reveal {
    if pending.is_empty() || !input_empty {
        return Reveal::None;
    }
    if pending.len() == 1 {
        Reveal::Ghost(pending.into_iter().next().unwrap().text)
    } else {
        Reveal::Chooser(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sugg(text: &str) -> Suggestion {
        Suggestion {
            text: text.into(),
            desc: None,
        }
    }

    #[test]
    fn parse_extracts_trims_and_drops_empties() {
        let v = parse_suggestions(&json!({ "replies": ["  open PR  ", "", "push"] }));
        assert_eq!(v, vec![sugg("open PR"), sugg("push")]);
    }

    #[test]
    fn parse_object_shape_carries_description() {
        let v = parse_suggestions(&json!({ "replies": [
            { "text": "  merge  ", "description": "  fast-forward  " },
            { "text": "rebase" },
            "squash",
        ] }));
        assert_eq!(
            v,
            vec![
                Suggestion {
                    text: "merge".into(),
                    desc: Some("fast-forward".into())
                },
                sugg("rebase"),
                sugg("squash"),
            ]
        );
    }

    #[test]
    fn parse_caps_at_five() {
        let v = parse_suggestions(&json!({ "replies": ["1","2","3","4","5","6","7"] }));
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn parse_malformed_is_empty() {
        assert!(parse_suggestions(&json!({})).is_empty());
        assert!(parse_suggestions(&json!({ "replies": "nope" })).is_empty());
        // Numbers / objects without `text` are skipped individually.
        assert!(parse_suggestions(&json!({ "replies": [1, 2] })).is_empty());
        assert!(
            parse_suggestions(&json!({ "replies": [{ "description": "no text" }] })).is_empty()
        );
    }

    #[test]
    fn reveal_single_is_ghost() {
        assert_eq!(
            plan_reveal(vec![sugg("only")], true),
            Reveal::Ghost("only".into())
        );
    }

    #[test]
    fn reveal_many_is_chooser() {
        assert_eq!(
            plan_reveal(vec![sugg("a"), sugg("b")], true),
            Reveal::Chooser(vec![sugg("a"), sugg("b")])
        );
    }

    #[test]
    fn reveal_skips_when_input_not_empty_or_pending_empty() {
        assert_eq!(plan_reveal(vec![sugg("a")], false), Reveal::None);
        assert_eq!(plan_reveal(Vec::new(), true), Reveal::None);
    }
}
