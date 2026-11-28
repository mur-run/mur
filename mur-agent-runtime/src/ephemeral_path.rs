//! Does a reply hand the user a path that dies with the turn (#2441)?
//!
//! A turn's track (`<project>/.worktrees/turn-*`) and its Cargo target dir
//! (`<project>/target/<subdir>/turn-*`) are reclaimed after the turn ends. A
//! reply that quotes a path under either gives the user something that will
//! not be there when they use it. The prompt rule tells the model not to; this
//! is the backstop that says so on the settlement card when it does anyway.
//!
//! Structural, like `external_state`: a path segment named `turn-<id>` whose
//! parent is the worktrees dir, or whose grandparent is the project target
//! dir. The target subdir is configurable, so it is not matched by name. A
//! glob such as `turn-*` is a description of the rule, not a handover, and
//! does not match.

use mur_track::turn::target::PROJECT_TARGET_DIR;
use mur_track::turn::{TRACK_PREFIX, WORKTREES_DIR};

/// Characters that end a path token in prose or Markdown.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '`' | '"' | '\'' | '(' | ')' | '<' | '>' | '[' | ']' | ','
        )
}

/// `seg` is `turn-<id>` with a concrete id: not a glob, and carrying a digit,
/// as every task id does — so `target/debug/turn-ish` is a binary, not a turn.
fn is_turn_segment(seg: &str) -> bool {
    seg.strip_prefix(TRACK_PREFIX).is_some_and(|id| {
        id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && id.chars().any(|c| c.is_ascii_digit())
    })
}

/// The token names a turn-scoped dir or something under it.
fn is_ephemeral(token: &str) -> bool {
    let segs: Vec<&str> = token.split(['/', '\\']).collect();
    segs.iter().enumerate().any(|(i, seg)| {
        is_turn_segment(seg)
            && ((i >= 1 && segs[i - 1] == WORKTREES_DIR)
                || (i >= 2 && segs[i - 2] == PROJECT_TARGET_DIR))
    })
}

/// Every distinct turn-scoped path the reply quotes, in order of appearance.
pub fn ephemeral_paths(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for token in text.split(is_delimiter) {
        // Trailing sentence punctuation is prose, not path.
        let token = token.trim_end_matches(['.', ':', ';', '。', '，', '：']);
        // `M=/path` in a shell line: the path is what is handed over.
        let token = token.rsplit_once('=').map_or(token, |(_, v)| v);
        if is_ephemeral(token) && !found.iter().any(|f| f == token) {
            found.push(token.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_target_binary_is_caught() {
        let text = "Run it:\n```\nM=/p/mur/target/turns/turn-01a1e6e2-1e64/debug/mur\n```";
        assert_eq!(
            ephemeral_paths(text),
            vec!["/p/mur/target/turns/turn-01a1e6e2-1e64/debug/mur"]
        );
    }

    #[test]
    fn a_turn_track_path_is_caught_in_prose_and_backticks() {
        let text = "報告在 `/p/.worktrees/turn-abc123/report.md`，請看。";
        assert_eq!(
            ephemeral_paths(text),
            vec!["/p/.worktrees/turn-abc123/report.md"]
        );
    }

    #[test]
    fn a_custom_target_subdir_is_still_caught() {
        assert_eq!(ephemeral_paths("see target/scratch/turn-9z/x").len(), 1);
    }

    #[test]
    fn globs_artifacts_and_lookalikes_are_not() {
        for text in [
            "never quote `.worktrees/turn-*` or `target/turns/turn-*`",
            "copied to ~/.mur/artifacts/mur/pr2426/mur",
            "the turn-based scheduler",
            "src/turn-ledger/mod.rs",
            "target/debug/turn-ish",
            "worktrees/turn-abc (no dot)",
        ] {
            assert!(ephemeral_paths(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn repeats_are_reported_once() {
        let p = "/p/.worktrees/turn-a1/x";
        assert_eq!(ephemeral_paths(&format!("{p} and again {p}.")), vec![p]);
    }
}
