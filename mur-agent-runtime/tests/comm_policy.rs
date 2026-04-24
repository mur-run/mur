use mur_agent_runtime::communication_policy::{accepts_from_allows, sends_to_allows};

#[test]
fn sends_to_wildcard_allows_all() {
    assert!(sends_to_allows(&["*".into()], "anyone"));
}

#[test]
fn sends_to_empty_denies() {
    assert!(!sends_to_allows(&[], "anyone"));
}

#[test]
fn accepts_from_glob_matches() {
    let list = vec!["notify_*".into(), "watcher".into()];
    assert!(accepts_from_allows(&list, "notify_a"));
    assert!(accepts_from_allows(&list, "watcher"));
    assert!(!accepts_from_allows(&list, "stranger"));
}
