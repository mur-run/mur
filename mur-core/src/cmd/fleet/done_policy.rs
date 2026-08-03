//! What a fleet's `done_when` string means.
//!
//! Three policies answer one question — "when is this fleet finished?" — and
//! the stored form is a single string so `fleet.yaml` keeps one field per
//! question. Parsing lives here rather than in `loop_run.rs` so the read side
//! (lenient: an unrecognised value means "ask the router", which keeps legacy
//! fleets loading) and the write side (strict: `settings.rs` rejects anything
//! outside the three forms) share one vocabulary.

/// The `done_when` sentinel selecting [`DonePolicy::QueueEmpty`].
pub const DONE_WHEN_QUEUE_EMPTY: &str = "queue-empty";

/// Prefix selecting [`DonePolicy::Marker`].
const MARKER_PREFIX: &str = "marker:";

/// How the guarded loop decides a fleet's goal is achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DonePolicy<'a> {
    /// Ask the router DONE/CONTINUE each iteration. The fallback: an empty
    /// criterion, or any legacy free-text one.
    Router,
    /// Stop as soon as an iteration finds no queued job. Deterministic and
    /// needs no cooperation from any agent.
    QueueEmpty,
    /// Converge when an agent emits this text as a whole line.
    Marker(&'a str),
}

/// Classify a stored `done_when`. Unrecognised values are [`DonePolicy::Router`]
/// rather than an error: `mur-common`'s own serde fixture carries
/// `done_when: 'all_tasks_done'`, and fleets written before this vocabulary
/// existed must keep loading.
pub fn done_policy(done_when: &str) -> DonePolicy<'_> {
    if let Some(marker) = done_marker(done_when) {
        return DonePolicy::Marker(marker);
    }
    if done_when.trim() == DONE_WHEN_QUEUE_EMPTY {
        return DonePolicy::QueueEmpty;
    }
    DonePolicy::Router
}

/// A structured `done_when` marker predicate: `marker:<TEXT>` means "converge
/// when a member emits `<TEXT>` as a sentinel (its own line) in the channel".
/// Returns the (trimmed, non-empty) marker text, or None for an empty /
/// non-`marker:` criterion (→ router fallback).
/// Machine-checkable convergence: deterministic and LLM-independent, vs. trusting
/// the router's free-text self-assessment.
///
/// Strips the prefix from the *trimmed* input — a hand-edited
/// `done_when: " marker:X"` must classify the same way here as it does in the
/// Hub's `parseDonePolicy` (which trims first), or the UI shows a policy the
/// loop isn't actually using.
pub fn done_marker(done_when: &str) -> Option<&str> {
    done_when
        .trim()
        .strip_prefix(MARKER_PREFIX)
        .map(str::trim)
        .filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_marker_parses_structured_criterion() {
        assert_eq!(done_marker("marker:FLEET_DONE"), Some("FLEET_DONE"));
        assert_eq!(done_marker("marker:  SHIPPED  "), Some("SHIPPED")); // trimmed
        assert_eq!(done_marker("marker:"), None); // empty
        assert_eq!(done_marker("marker:   "), None); // whitespace only
        assert_eq!(done_marker("all tasks closed"), None); // free text → router fallback
        assert_eq!(done_marker(""), None);
        // Leading whitespace before the prefix (a hand-edited fleet.yaml) must
        // still classify as a marker — matches the Hub's `parseDonePolicy`,
        // which trims before checking the prefix.
        assert_eq!(done_marker("  marker:FLEET_DONE"), Some("FLEET_DONE"));
    }

    #[test]
    fn done_policy_maps_the_three_forms_and_treats_legacy_values_as_router() {
        assert_eq!(done_policy("marker:DONE"), DonePolicy::Marker("DONE"));
        assert_eq!(done_policy(DONE_WHEN_QUEUE_EMPTY), DonePolicy::QueueEmpty);
        assert_eq!(done_policy("  queue-empty  "), DonePolicy::QueueEmpty);
        assert_eq!(done_policy(""), DonePolicy::Router);
        // mur-common's serde fixture carries exactly this shape; it must keep
        // meaning "ask the router" rather than erroring or half-matching.
        assert_eq!(done_policy("all_tasks_done"), DonePolicy::Router);
    }
}
