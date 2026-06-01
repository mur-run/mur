use mur_common::route::TaskType;
use mur_core::route::heuristic::{DefaultHeuristic, DifficultyHeuristic};

#[test]
fn heuristic_scores_low_for_execution() {
    let h = DefaultHeuristic::default();
    let score = h.score("run cargo test", TaskType::Execution, 200);
    assert!(score < 0.5, "execution should score low, got {score}");
}

#[test]
fn heuristic_scores_high_for_refactor() {
    let h = DefaultHeuristic::default();
    let score = h.score(
        "refactor the auth module to use the new token format across all handlers",
        TaskType::Refactor,
        5000,
    );
    assert!(score > 0.5, "refactor should score high, got {score}");
}

#[test]
fn heuristic_scores_higher_for_more_tokens() {
    let h = DefaultHeuristic::default();
    let small = h.score("fix typo in README", TaskType::Documentation, 100);
    let large = h.score("fix typo in README", TaskType::Documentation, 10000);
    assert!(
        large > small,
        "more tokens should increase score: {small} vs {large}"
    );
}
