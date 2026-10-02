use super::*;

#[test]
fn exact_workflow_name_matches() {
    let names = vec!["deploy-production".to_owned()];
    assert!(workflow_name_matches_query(
        "deploy the production service",
        &names
    ));
}

#[test]
fn partial_workflow_name_matches() {
    let names = vec!["search-bookstore".to_owned()];
    assert!(workflow_name_matches_query(
        "search for latest books",
        &names
    ));
}

#[test]
fn unrelated_query_does_not_match() {
    let names = vec!["deploy-production".to_owned()];
    assert!(!workflow_name_matches_query("fix the lint error", &names));
}

#[test]
fn short_words_are_ignored() {
    let names = vec!["run-ci".to_owned()];
    // "run" (3 chars) and "ci" (2 chars) — both < 4 chars, no match
    assert!(!workflow_name_matches_query("run ci now", &names));
}

#[test]
fn empty_workflow_list_never_matches() {
    assert!(!workflow_name_matches_query(
        "deploy production service",
        &[]
    ));
}
