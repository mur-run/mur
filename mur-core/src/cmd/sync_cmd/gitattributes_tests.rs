use super::ensure_union_merge;

/// Runs on every sync, so it has to be idempotent and must not clobber a
/// `.gitattributes` the user wrote for their own reasons.
#[test]
fn union_merge_rule_is_added_once_and_preserves_existing_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".gitattributes");
    std::fs::write(&path, "*.png binary").unwrap();

    ensure_union_merge(dir.path()).unwrap();
    ensure_union_merge(dir.path()).unwrap();

    let body = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        body.matches("merge=union").count(),
        1,
        "rule must not stack up: {body}"
    );
    assert!(body.contains("*.png binary"), "clobbered: {body}");
    assert!(body.ends_with('\n'), "no trailing newline: {body:?}");
}
