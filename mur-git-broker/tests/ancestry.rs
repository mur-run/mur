// tests/ancestry.rs — real git.
use mur_git_broker::{
    ancestry::judge, error::BrokerError, oid::*, policy::BrokerLimits, repo::PrivateRepo,
};
mod common;
use common::*;

/// work repo with: A ← B (descendant) and orphan S (sibling of A). Private repo fetched from work.
struct Fx {
    _t: tempfile::TempDir,
    repo: PrivateRepo,
    a: String,
    b: String,
    s: String,
}
fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    let a = commit(&w, "a");
    let b = commit(&w, "b");
    git(&w, &["checkout", "-q", "--orphan", "side"]);
    let s = commit(&w, "s");
    let repo = PrivateRepo::create(&t.path().join("p"), ObjectFormat::Sha1, &git_bin()).unwrap();
    for (i, c) in [&a, &b, &s].iter().enumerate() {
        git(&w, &["update-ref", &format!("refs/keep/{i}"), c]);
    }
    let o = repo
        .runner()
        .run(
            &[
                "fetch",
                "-q",
                "--no-tags",
                w.to_str().unwrap(),
                "+refs/keep/*:refs/prefetch/k/*",
            ],
            std::time::Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(o.code, 0, "{}", String::from_utf8_lossy(&o.stderr));
    Fx {
        _t: t,
        repo,
        a,
        b,
        s,
    }
}
fn act(old: &str, new: &str) -> mur_git_broker::action::ActionDocument {
    action_for(old, new, "refs/heads/agent/x")
}
fn l() -> BrokerLimits {
    BrokerLimits::default()
}

#[test]
fn descendant_is_fast_forward() {
    let f = fx();
    judge(&f.repo, &act(&f.a, &f.b), &l()).unwrap();
}
#[test]
fn sibling_is_not_fast_forward() {
    let f = fx();
    assert_eq!(
        judge(&f.repo, &act(&f.a, &f.s), &l()),
        Err(BrokerError::NotFastForward)
    );
}
#[test]
fn replace_ref_cannot_forge_ancestry() {
    let f = fx();
    // measured: with refs/replace/S → B, plain `merge-base --is-ancestor A S` says 0.
    let o = f
        .repo
        .runner()
        .run(
            &["update-ref", &format!("refs/replace/{}", f.s), &f.b],
            std::time::Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(o.code, 0);
    assert_eq!(
        judge(&f.repo, &act(&f.a, &f.s), &l()),
        Err(BrokerError::NotFastForward),
        "GIT_NO_REPLACE_OBJECTS=1 in the runner env must ignore the replace ref"
    );
}
#[test]
fn grafts_are_rejected_not_deleted() {
    let f = fx();
    let g = f.repo.path().join("info/grafts");
    std::fs::create_dir_all(g.parent().unwrap()).unwrap();
    std::fs::write(&g, format!("{} {}\n", f.s, f.a)).unwrap();
    assert!(matches!(
        judge(&f.repo, &act(&f.a, &f.s), &l()),
        Err(BrokerError::AncestryUnprovable(_))
    ));
    assert!(g.exists());
}
#[test]
fn shallow_and_commit_graph_are_unprovable() {
    for p in ["shallow", "objects/info/commit-graph"] {
        let f = fx();
        let path = f.repo.path().join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        assert!(
            matches!(
                judge(&f.repo, &act(&f.a, &f.b), &l()),
                Err(BrokerError::AncestryUnprovable(_))
            ),
            "{p}"
        );
    }
}
#[test]
fn missing_intermediate_commit_is_unprovable() {
    let f = fx();
    // new_sha names an object the private repo does not have ⇒ git exits 128, never 1.
    let e = judge(&f.repo, &act(&f.a, &"c".repeat(40)), &l());
    assert!(
        matches!(e, Err(BrokerError::AncestryUnprovable(_))),
        "{e:?}"
    );
}
#[test]
fn killed_or_timed_out_git_is_unprovable() {
    use mur_git_broker::{ancestry::classify, git::GitError};
    for e in [
        GitError::Timeout,
        GitError::Signal,
        GitError::Spawn("x".into()),
    ] {
        assert!(matches!(
            classify(Err(e)),
            Err(BrokerError::AncestryUnprovable(_))
        ));
    }
}
#[test]
fn creation_skips_the_check() {
    let f = fx(); // old = zero OID; new need not even exist, because no git command runs
    judge(
        &f.repo,
        &act(&zero_oid(ObjectFormat::Sha1), &"d".repeat(40)),
        &l(),
    )
    .unwrap();
}
#[test]
fn only_exit_0_and_1_are_meaningful() {
    use mur_git_broker::{ancestry::classify, git::GitOutput};
    let out = |code| {
        Ok(GitOutput {
            code,
            stdout: vec![],
            stderr: vec![],
        })
    };
    assert_eq!(classify(out(0)), Ok(()));
    assert_eq!(classify(out(1)), Err(BrokerError::NotFastForward));
    for c in [2, 128, 129, 255, -1] {
        assert!(
            matches!(classify(out(c)), Err(BrokerError::AncestryUnprovable(_))),
            "exit {c}"
        );
    }
    let f = fx(); // end to end: an unparsable revision exits 128 ⇒ unprovable, never "not a fast-forward"
    assert!(matches!(
        judge(&f.repo, &act("not-an-oid", &f.b), &l()),
        Err(BrokerError::AncestryUnprovable(_))
    ));
}
