use super::orphan_should_prune;

#[test]
fn prunes_removed_worktree_but_never_on_unmount() {
    // Linked worktree removed while the repo is still mounted → reclaim.
    assert!(orphan_should_prune(
        "/repo",
        "/repo/.worktrees/x",
        true,
        false
    ));
    // External drive unmounted: the repo root is ALSO gone → ambiguous → KEEP.
    assert!(!orphan_should_prune(
        "/repo",
        "/repo/.worktrees/x",
        false,
        false
    ));
    // Both present → nothing to GC.
    assert!(!orphan_should_prune(
        "/repo",
        "/repo/.worktrees/x",
        true,
        true
    ));
    // Legacy metadata (no repo_root recorded) → never auto-prune.
    assert!(!orphan_should_prune("", "/repo/.worktrees/x", true, false));
}
