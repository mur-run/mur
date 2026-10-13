use super::*;
use mur_common::hitl::RiskTier;
use serde_json::json;

/// A throwaway repo whose `origin` is mur-run/mur, on branch `fix/x`.
fn repo() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let r = git2::Repository::init(t.path()).unwrap();
    r.remote("origin", "git@github.com:mur-run/mur.git")
        .unwrap();
    let sig = git2::Signature::now("t", "t@t").unwrap();
    let tree = r
        .find_tree(r.index().unwrap().write_tree().unwrap())
        .unwrap();
    let c = r.commit(None, &sig, &sig, "i", &tree, &[]).unwrap();
    let c = r.find_commit(c).unwrap();
    r.branch("fix/x", &c, true).unwrap();
    r.set_head("refs/heads/fix/x").unwrap();
    t
}

fn bash_at(t: &tempfile::TempDir, rest: &str) -> serde_json::Value {
    json!({ "command": format!("cd '{}' && {rest}", t.path().display()) })
}

/// The Hub must store exactly the key the CLI stores, so the egress key is the
/// resolved URL — not `bash`, and not the remote's name.
#[test]
fn a_feature_push_is_keyed_on_the_resolved_url() {
    let t = repo();
    let g = session_grant("bash", &bash_at(&t, "git push -u origin fix/x"), None)
        .expect("a feature-branch push is grantable for the session");
    assert_eq!(g.key, "egress:git-push:github.com/mur-run/mur:unprotected");
    assert!(g.label.contains("github.com/mur-run/mur"), "{}", g.label);
}

/// The calls #1835 says must keep asking get no key, so the Hub cannot even
/// offer the row for them.
#[test]
fn force_pushes_and_protected_branches_have_no_key() {
    let t = repo();
    for cmd in [
        "git push --force origin fix/x",
        "git push -f origin fix/x",
        "git push origin +fix/x",
        "git push origin main",
        "git push origin master",
        "gh pr merge 1486",
    ] {
        assert_eq!(
            session_grant("bash", &bash_at(&t, cmd), None),
            None,
            "must keep asking: {cmd}"
        );
    }
}

/// A tier above the ceiling with no proved scope is not grantable; the same
/// tool within the ceiling is keyed on its name, as in the CLI.
#[test]
fn the_tier_ceiling_decides_tool_name_grants() {
    assert_eq!(
        session_grant("bash", &json!({ "command": "rm -rf build" }), None),
        None
    );
    let g = session_grant("write_file", &json!({ "path": "/x/y" }), None)
        .expect("a write-tier tool is grantable by name");
    assert_eq!(g.key, "write_file");
}

/// A declared `risk:` only raises the tier: a profile marking a tool
/// destructive removes the session row the classifier alone would offer.
#[test]
fn a_declared_risk_can_remove_the_grant() {
    let input = json!({ "path": "/x/y" });
    assert!(session_grant("write_file", &input, None).is_some());
    assert_eq!(
        session_grant("write_file", &input, Some(RiskTier::Destructive)),
        None
    );
}
