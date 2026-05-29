use crate::nudge::candidate::WorkflowCandidate;

/// Message id namespace so the response drain can recognize nudge replies.
pub const NUDGE_ID_PREFIX: &str = "nudge:";

pub fn nudge_msg_id(candidate_id: &str) -> String {
    format!("{NUDGE_ID_PREFIX}{candidate_id}")
}

pub fn candidate_id_from_msg(msg_id: &str) -> Option<String> {
    msg_id.strip_prefix(NUDGE_ID_PREFIX).map(|s| s.to_string())
}

/// Deterministic, locale-aware nudge body. English default for v1.
pub fn nudge_body(c: &WorkflowCandidate, _locale: &str) -> String {
    format!(
        "I noticed you did this across {} sessions:\n\n  {}\n\nWant me to save it as a replayable workflow you can run with `mur run {}`?",
        c.session_count, c.title, c.suggested_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;

    fn cand() -> WorkflowCandidate {
        WorkflowCandidate {
            id: "abc123".into(),
            title: "Run tests, then commit, then push".into(),
            suggested_name: "test-commit-push".into(),
            steps_preview: vec!["cargo test".into(), "git commit".into()],
            session_count: 4,
            evidence_session_ids: vec![],
        }
    }

    #[test]
    fn body_mentions_title_and_count() {
        let b = nudge_body(&cand(), "en");
        assert!(b.contains("Run tests, then commit, then push"));
        assert!(b.contains("4")); // session count
    }

    #[test]
    fn message_id_encodes_candidate_id() {
        assert_eq!(nudge_msg_id("abc123"), "nudge:abc123");
        assert_eq!(
            candidate_id_from_msg("nudge:abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(candidate_id_from_msg("other"), None);
    }
}
