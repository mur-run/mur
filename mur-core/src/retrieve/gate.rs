//! Adaptive query gate — decides whether to trigger pattern retrieval.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, PartialEq)]
pub enum GateDecision {
    /// Skip retrieval — query is noise
    Skip(String),
    /// Force retrieval — high-value query
    Force,
    /// Normal retrieval
    Pass,
}

static SKIP_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)^(hi|hello|hey|yo|sup|thanks|thank you|ok|okay|sure|yes|no|bye|quit|exit)[\s!.]*$").unwrap(),
        Regex::new(r"^[\p{Emoji}\s]+$").unwrap(),
        Regex::new(r"(?i)^(cd|ls|pwd|cat|mkdir|rm|cp|mv|git\s+(status|log|diff|add|commit|push|pull))\b").unwrap(),
    ]
});

static FORCE_KEYWORDS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        "error",
        "fail",
        "bug",
        "fix",
        "crash",
        "exception",
        "panic",
        "remember",
        "上次",
        "之前",
        "以前",
        "怎麼",
        "how to",
        "how do",
        "best practice",
        "convention",
        "pattern",
    ]
});

/// Evaluate whether a query should trigger pattern retrieval.
pub fn evaluate_query(query: &str) -> GateDecision {
    let trimmed = query.trim();

    // Too short
    if trimmed.chars().count() < 3 {
        return GateDecision::Skip("too short".into());
    }

    // Check skip patterns
    for pat in SKIP_PATTERNS.iter() {
        if pat.is_match(trimmed) {
            return GateDecision::Skip("noise pattern".into());
        }
    }

    // Check force keywords
    let lower = trimmed.to_lowercase();
    for kw in FORCE_KEYWORDS.iter() {
        if lower.contains(kw) {
            return GateDecision::Force;
        }
    }

    GateDecision::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_greetings() {
        assert_eq!(evaluate_query("hi"), GateDecision::Skip("too short".into()));
        assert!(matches!(evaluate_query("hello!"), GateDecision::Skip(_)));
        assert!(matches!(evaluate_query("thanks"), GateDecision::Skip(_)));
    }

    #[test]
    fn test_skip_emoji() {
        assert!(matches!(evaluate_query("👍"), GateDecision::Skip(_)));
        assert!(matches!(evaluate_query("🎉 🚀"), GateDecision::Skip(_)));
    }

    #[test]
    fn test_skip_commands() {
        assert!(matches!(
            evaluate_query("git status"),
            GateDecision::Skip(_)
        ));
        assert!(matches!(evaluate_query("ls -la"), GateDecision::Skip(_)));
    }

    #[test]
    fn test_force_error() {
        assert_eq!(
            evaluate_query("I got an error with swift build"),
            GateDecision::Force
        );
        assert_eq!(evaluate_query("how to fix this crash"), GateDecision::Force);
    }

    #[test]
    fn test_force_cjk() {
        assert_eq!(evaluate_query("上次怎麼解決的"), GateDecision::Force);
        assert_eq!(evaluate_query("之前的做法是什麼"), GateDecision::Force);
    }

    #[test]
    fn test_pass_normal() {
        assert_eq!(
            evaluate_query("implement a REST API for users"),
            GateDecision::Pass
        );
        assert_eq!(
            evaluate_query("refactor the auth module"),
            GateDecision::Pass
        );
    }

    #[test]
    fn test_too_short() {
        assert!(matches!(evaluate_query("ab"), GateDecision::Skip(_)));
        assert!(matches!(evaluate_query(""), GateDecision::Skip(_)));
    }
}
