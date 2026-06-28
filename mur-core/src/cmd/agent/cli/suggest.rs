//! Pure logic for agent-suggested replies: extract the `replies` array from a
//! `suggest_replies` tool call, and decide how to reveal them. No TUI, no I/O.

/// Hard cap on suggestions shown (matches the tool schema's maxItems).
pub const MAX_SUGGESTIONS: usize = 5;

/// Extract non-empty reply strings from the tool-call args, capped. Fail-soft:
/// any malformed shape yields an empty vec.
pub fn parse_suggestions(args: &serde_json::Value) -> Vec<String> {
    let Some(arr) = args.get("replies").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take(MAX_SUGGESTIONS)
        .map(str::to_string)
        .collect()
}

/// How to surface a set of pending suggestions.
#[derive(Debug, Clone, PartialEq)]
pub enum Reveal {
    /// Nothing to show (empty, or the composer already has text).
    None,
    /// A single suggestion → ghost placeholder text.
    Ghost(String),
    /// Two or more → a chooser overlay.
    Chooser(Vec<String>),
}

/// Decide how to reveal `pending` given whether the composer is empty.
pub fn plan_reveal(pending: Vec<String>, input_empty: bool) -> Reveal {
    if pending.is_empty() || !input_empty {
        return Reveal::None;
    }
    if pending.len() == 1 {
        Reveal::Ghost(pending.into_iter().next().unwrap())
    } else {
        Reveal::Chooser(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_extracts_trims_and_drops_empties() {
        let v = parse_suggestions(&json!({ "replies": ["  open PR  ", "", "push"] }));
        assert_eq!(v, vec!["open PR".to_string(), "push".to_string()]);
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
        assert!(parse_suggestions(&json!({ "replies": [1, 2] })).is_empty());
    }

    #[test]
    fn reveal_single_is_ghost() {
        assert_eq!(
            plan_reveal(vec!["only".into()], true),
            Reveal::Ghost("only".into())
        );
    }

    #[test]
    fn reveal_many_is_chooser() {
        assert_eq!(
            plan_reveal(vec!["a".into(), "b".into()], true),
            Reveal::Chooser(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn reveal_skips_when_input_not_empty_or_pending_empty() {
        assert_eq!(plan_reveal(vec!["a".into()], false), Reveal::None);
        assert_eq!(plan_reveal(Vec::new(), true), Reveal::None);
    }
}
