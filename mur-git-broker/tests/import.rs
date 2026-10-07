#![cfg(unix)]
// tests/import.rs — real git.
use mur_git_broker::{
    constants::*,
    error::BrokerError,
    git::{GitError, GitOutput},
    import::*,
    oid::*,
    policy::BrokerLimits,
    repo::PrivateRepo,
};
use std::{path::Path, process::Command, time::Duration};
mod common;
use common::*;

const T: Duration = Duration::from_secs(DEFAULT_GIT_TIMEOUT_SECS);
fn priv_repo(root: &Path) -> PrivateRepo {
    PrivateRepo::create(root, ObjectFormat::Sha1, &git_bin()).unwrap()
}
fn prefetch(remote: &Path, repo: &PrivateRepo, spec: &str) {
    let o = repo
        .runner()
        .run(
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--no-write-fetch-head",
                remote.to_str().unwrap(),
                spec,
            ],
            T,
        )
        .unwrap();
    assert_eq!(o.code, 0, "{}", String::from_utf8_lossy(&o.stderr));
}
/// Fixture: remote has OLD on agent/x; work has NEW (child of OLD). Private repo prefetched OLD.
struct Fx {
    _t: tempfile::TempDir,
    work: std::path::PathBuf,
    _remote: std::path::PathBuf,
    repo: PrivateRepo,
    old: String,
    new: String,
}
fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let old = commit(&work, "old");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    let new = commit(&work, "new");
    let repo = priv_repo(&t.path().join("p"));
    prefetch(&remote, &repo, "+refs/heads/agent/x:refs/prefetch/old");
    Fx {
        _t: t,
        work,
        _remote: remote,
        repo,
        old,
        new,
    }
}
fn pack_files(repo: &PrivateRepo) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(repo.path().join("objects/pack"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn range_pack_with_prefetch_imports_and_closes() {
    // F33 happy path
    let f = fx();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    import_pack(&f.repo, &pack, &a, &BrokerLimits::default(), &RlimitSpawn).unwrap();
    closure_check(&f.repo, &a).unwrap();
    assert_eq!(pack_files(&f.repo), ["pack.idx", "pack.pack", "pack.rev"]);
}
#[test]
fn range_pack_without_prefetch_is_rejected() {
    let f = fx();
    let empty = priv_repo(&f._t.path().join("empty"));
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    let e = import_pack(&empty, &pack, &a, &BrokerLimits::default(), &RlimitSpawn);
    assert!(matches!(e, Err(BrokerError::ImportRejected(_))), "{e:?}"); // measured: exit 128
}
#[test]
fn thin_pack_is_import_rejected() {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    std::fs::write(
        work.join("big"),
        (1..4000).map(|i| i.to_string() + "\n").collect::<String>(),
    )
    .unwrap();
    git(&work, &["add", "."]);
    let old = commit(&work, "old");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    let mut s = std::fs::read_to_string(work.join("big")).unwrap();
    s.push_str("tail\n");
    std::fs::write(work.join("big"), s).unwrap();
    git(&work, &["commit", "-qam", "new"]);
    let new = git(&work, &["rev-parse", "HEAD"]);
    let repo = priv_repo(&t.path().join("p"));
    prefetch(&remote, &repo, "+refs/heads/agent/x:refs/prefetch/old");
    // Make the base unavailable so the delta cannot resolve: empty private repo, no prefetch.
    let bare = priv_repo(&t.path().join("q"));
    let pack = range_pack(&work, &new, Some(&old), true);
    let a = action_for(&old, &new, "refs/heads/agent/x");
    let e = import_pack(&bare, &pack, &a, &BrokerLimits::default(), &RlimitSpawn);
    assert!(matches!(e, Err(BrokerError::ImportRejected(_))));
    assert!(
        !format!("{:?}", std::fs::read_to_string(bare.path().join("config"))).contains("fix-thin")
    );
}
/// Parser double: runs the real command, then tries to plant a hook and a config rewrite in the repo.
struct Malicious<'a> {
    repo: &'a Path,
}
impl ParserSpawn for Malicious<'_> {
    fn spawn(&self, cmd: Command, out: &Path, l: &BrokerLimits) -> Result<GitOutput, GitError> {
        let r = RlimitSpawn.spawn(cmd, out, l);
        let _ = std::fs::write(self.repo.join("hooks/pre-push"), b"#!/bin/sh\n");
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(self.repo.join("config"))
            .and_then(|mut c| {
                use std::io::Write;
                c.write_all(b"[url \"X\"]\n\tinsteadOf = Y\n")
            });
        std::fs::write(out.join("evil.sh"), b"x").unwrap();
        std::fs::write(out.join("config"), b"x").unwrap();
        r
    }
}
#[test]
fn parser_cannot_write_the_private_repo_and_broker_moves_only_pack_files() {
    let f = fx();
    f.repo.freeze().unwrap(); // production order: control files are frozen first
    let before = f.repo.control_digest().unwrap();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    import_pack(
        &f.repo,
        &pack,
        &a,
        &BrokerLimits::default(),
        &Malicious {
            repo: f.repo.path(),
        },
    )
    .unwrap();
    assert_eq!(
        before,
        f.repo.control_digest().unwrap(),
        "config/hooks untouched"
    );
    assert_eq!(
        std::fs::read_dir(f.repo.path().join("hooks"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        pack_files(&f.repo),
        ["pack.idx", "pack.pack", "pack.rev"],
        "evil.sh / config stayed in out/"
    );
}
#[test]
fn oversize_pack_is_rejected_before_parsing() {
    let f = fx();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    struct Never;
    impl ParserSpawn for Never {
        fn spawn(&self, _: Command, _: &Path, _: &BrokerLimits) -> Result<GitOutput, GitError> {
            panic!("parser ran")
        }
    }
    let l = BrokerLimits {
        max_pack_bytes: 1,
        ..Default::default()
    };
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    assert!(matches!(
        import_pack(&f.repo, &pack, &a, &l, &Never),
        Err(BrokerError::ImportRejected(_))
    ));
}
#[test]
fn too_many_objects_is_rejected() {
    let f = fx();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false); // 2 objects: commit + tree? (empty tree already in OLD ⇒ 1)
    let l = BrokerLimits {
        max_object_count: 0,
        ..Default::default()
    };
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    let e = import_pack(&f.repo, &pack, &a, &l, &RlimitSpawn);
    assert!(matches!(e, Err(BrokerError::ImportRejected(_))));
    assert_eq!(
        pack_files(&f.repo),
        Vec::<String>::new(),
        "nothing moved in on rejection"
    );
}
#[test]
fn oversize_blob_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let old = commit(&work, "old");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    std::fs::write(work.join("blob"), vec![b'z'; 200_000]).unwrap();
    git(&work, &["add", "."]);
    let new = commit(&work, "new");
    let repo = priv_repo(&t.path().join("p"));
    prefetch(&remote, &repo, "+refs/heads/agent/x:refs/prefetch/old");
    let pack = range_pack(&work, &new, Some(&old), false);
    let l = BrokerLimits {
        max_blob_bytes: 1000,
        ..Default::default()
    };
    let a = action_for(&old, &new, "refs/heads/agent/x");
    assert!(matches!(
        import_pack(&repo, &pack, &a, &l, &RlimitSpawn),
        Err(BrokerError::ImportRejected(_))
    ));
}
#[test]
fn update_closure_needs_old_and_new_to_be_commits() {
    let f = fx();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    let tree = git(&f.work, &["rev-parse", &format!("{}^{{tree}}", f.old)]);
    let a = action_for(&tree, &f.new, "refs/heads/agent/x"); // a tree OID as old_sha
    let _ = import_pack(&f.repo, &pack, &a, &BrokerLimits::default(), &RlimitSpawn);
    assert!(matches!(
        closure_check(&f.repo, &a),
        Err(BrokerError::ImportRejected(_))
    ));
}
#[test]
fn creation_closure_fails_before_import_and_passes_after() {
    // measured: 128 → 0
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let base = commit(&work, "base");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{base}:refs/heads/main"),
        ],
    );
    let new = commit(&work, "feature");
    let repo = priv_repo(&t.path().join("p"));
    prefetch(&remote, &repo, "+refs/heads/main:refs/prefetch/base/main");
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &new, "refs/heads/agent/new");
    assert!(matches!(
        closure_check(&repo, &a),
        Err(BrokerError::ImportRejected(_))
    ));
    import_pack(
        &repo,
        &range_pack(&work, &new, Some(&base), false),
        &a,
        &BrokerLimits::default(),
        &RlimitSpawn,
    )
    .unwrap();
    closure_check(&repo, &a).unwrap();
}
#[test]
fn creation_whose_parent_is_nowhere_is_rejected() {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let base = commit(&work, "base");
    let mid = commit(&work, "mid");
    let new = commit(&work, "new");
    let remote = bare_remote(t.path());
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{base}:refs/heads/main"),
        ],
    );
    let repo = priv_repo(&t.path().join("p"));
    prefetch(&remote, &repo, "+refs/heads/main:refs/prefetch/base/main");
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &new, "refs/heads/agent/new");
    // pack carries only `new`; its parent `mid` is on neither the remote nor in the pack.
    let e = import_pack(
        &repo,
        &range_pack(&work, &new, Some(&mid), false),
        &a,
        &BrokerLimits::default(),
        &RlimitSpawn,
    );
    assert!(matches!(e, Err(BrokerError::ImportRejected(_))), "{e:?}"); // strict index-pack exits 128
}
#[test]
fn creation_with_no_base_refs_needs_a_self_contained_pack() {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let _root = commit(&work, "root");
    let new = commit(&work, "tip");
    let repo = priv_repo(&t.path().join("p")); // nothing prefetched
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &new, "refs/heads/agent/new");
    import_pack(
        &repo,
        &range_pack(&work, &new, None, false),
        &a,
        &BrokerLimits::default(),
        &RlimitSpawn,
    )
    .unwrap();
    closure_check(&repo, &a).unwrap();
}
#[test]
fn idx_object_count_reads_the_fanout() {
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    for i in 0..3 {
        std::fs::write(work.join(format!("f{i}")), i.to_string()).unwrap();
        git(&work, &["add", "."]);
        commit(&work, "c");
    }
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let repo = priv_repo(&t.path().join("p"));
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &tip, "refs/heads/agent/n");
    import_pack(
        &repo,
        &range_pack(&work, &tip, None, false),
        &a,
        &BrokerLimits::default(),
        &RlimitSpawn,
    )
    .unwrap();
    let want: u64 = git(&work, &["rev-list", "--objects", &tip]).lines().count() as u64;
    assert_eq!(
        idx_object_count(&repo.path().join("objects/pack/pack.idx")).unwrap(),
        want
    );
    assert!(
        idx_object_count(&work.join("f0")).is_err(),
        "not an idx ⇒ InvalidData, not a panic"
    );
}
#[test]
fn every_rejection_reason_lets_the_caller_destroy_the_repo() {
    // The orchestrator owns cleanup; this pins the contract: after Err, `destroy()` leaves no path.
    let f = fx();
    let pack = range_pack(&f.work, &f.new, Some(&f.old), false);
    let l = BrokerLimits {
        max_pack_bytes: 1,
        ..Default::default()
    };
    let a = action_for(&f.old, &f.new, "refs/heads/agent/x");
    assert!(import_pack(&f.repo, &pack, &a, &l, &RlimitSpawn).is_err());
    let p = f.repo.path().to_path_buf();
    f.repo.destroy();
    assert!(!p.exists());
}

#[test]
fn concurrent_imports_sharing_a_root_do_not_collide() {
    // Two repos under one root, imported from two threads of the same process: the scratch dir
    // name must be unique per import, not per process.
    let t = tempfile::tempdir().unwrap();
    let work = work_repo(t.path());
    let tip = commit(&work, "only");
    let pack = range_pack(&work, &tip, None, false);
    let a = action_for(&zero_oid(ObjectFormat::Sha1), &tip, "refs/heads/agent/n");
    let r1 = PrivateRepo::create(&t.path().join("one"), ObjectFormat::Sha1, &git_bin()).unwrap();
    let r2 = PrivateRepo::create(&t.path().join("two"), ObjectFormat::Sha1, &git_bin()).unwrap();
    std::thread::scope(|s| {
        for r in [&r1, &r2] {
            s.spawn(|| {
                import_pack(r, &pack, &a, &BrokerLimits::default(), &RlimitSpawn).unwrap();
            });
        }
    });
}
