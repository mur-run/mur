//! Classify a failed run as a *workflow* failure (the skill is broken) vs an
//! *environment* failure (network/credentials/missing binary) so a flaky
//! network never marks a workflow Broken (workflow-engine v2, Layer 4).
//!
//! Classification is heuristic; the confidence field lets the Broken
//! fast-path (P4) require high-confidence workflow failures before demoting,
//! and lets the user override via `mur run --env-class workflow|env`.

/// Result of classifying a failure's stderr/output.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvClassification {
    /// "workflow" | "env"
    pub class: &'static str,
    pub confidence: f64,
}

/// Substrings that indicate the *environment* failed, not the workflow.
const ENV_MARKERS: &[&str] = &[
    "connection refused",
    "connection reset",
    "timed out",
    "timeout",
    "could not resolve",
    "temporary failure in name resolution",
    "network is unreachable",
    "tls",
    "certificate",
    "401",
    "403",
    "unauthorized",
    "forbidden",
    "credential",
    "permission denied",
    "no such file or directory",
    "command not found",
    "not found in path",
    "rate limit",
    "429",
    "disk full",
    "no space left",
];

/// Heuristic stderr classifier. Zero env markers → likely a workflow bug
/// (moderate confidence); one marker → likely environmental; several → almost
/// certainly environmental.
pub fn classify_failure(stderr: &str) -> EnvClassification {
    let lower = stderr.to_lowercase();
    let hits = ENV_MARKERS.iter().filter(|m| lower.contains(*m)).count();
    match hits {
        0 => EnvClassification {
            class: "workflow",
            confidence: 0.6,
        },
        1 => EnvClassification {
            class: "env",
            confidence: 0.6,
        },
        _ => EnvClassification {
            class: "env",
            confidence: 0.9,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_assertion_failure_is_workflow() {
        let c = classify_failure("assertion failed: expected 3 got 2");
        assert_eq!(c.class, "workflow");
    }

    #[test]
    fn network_error_is_env() {
        let c = classify_failure("curl: (7) Connection refused");
        assert_eq!(c.class, "env");
    }

    #[test]
    fn multiple_markers_high_confidence_env() {
        let c = classify_failure("error: timed out\ncurl: connection refused after retry");
        assert_eq!(c.class, "env");
        assert!(c.confidence > 0.8);
    }
}
