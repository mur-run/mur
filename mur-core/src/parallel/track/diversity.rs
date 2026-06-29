//! Approach suffix injected into agent system prompts for track diversity.

use mur_common::parallel::TrackConfig;

/// Returns a block of text to append to an agent's system prompt,
/// steering it toward the desired approach for this track.
pub fn approach_system_suffix(tc: &TrackConfig) -> String {
    format!(
        "\n---\n## Parallel Track: {}\n\nYou are implementing in track `{}`. Approach:\n{}\n",
        tc.name,
        tc.name,
        tc.approach.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn suffix_contains_approach_text() {
        let tc = TrackConfig {
            name: "track-functional".into(),
            approach: "Prefer functional style: Iterator combinators, avoid mutable state.".into(),
            model: None,
        };
        let suffix = approach_system_suffix(&tc);
        assert!(suffix.contains("track-functional"));
        assert!(suffix.contains("Iterator combinators"));
        assert!(suffix.ends_with('\n'));
    }

    #[test]
    fn suffix_is_stable_across_calls() {
        let tc = TrackConfig {
            name: "t".into(),
            approach: "performance first".into(),
            model: None,
        };
        assert_eq!(approach_system_suffix(&tc), approach_system_suffix(&tc));
    }
}
