#![cfg(unix)]
// tests/prefetch.rs — real git; remote is a local bare repo.
use mur_git_broker::{error::BrokerError, oid::*, policy::*, prefetch::*, repo::PrivateRepo};
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
mod common;
use common::*; // work_repo, commit, git, bare_remote, action_for(old,new,ref), policy()

fn priv_repo(root: &Path) -> PrivateRepo {
    PrivateRepo::create(root, ObjectFormat::Sha1, &git_bin()).unwrap()
}

#[test]
fn update_prefetches_only_the_target_ref() {
    let a = action_for(&"a".repeat(40), &"b".repeat(40), "refs/heads/agent/x");
    let s = prefetch_specs(&a, &policy());
    assert_eq!(s, vec!["+refs/heads/agent/x:refs/prefetch/old".to_string()]);
    for x in &s {
        assert!(!x.contains('*') && x != "--all" && x != "--mirror");
    }
}
#[test]
fn creation_prefetches_only_creation_base_refs() {
    let a = action_for(
        &zero_oid(ObjectFormat::Sha1),
        &"b".repeat(40),
        "refs/heads/agent/x",
    );
    let mut p = policy();
    p.creation_base_refs = vec!["refs/heads/main".into()];
    assert_eq!(
        prefetch_specs(&a, &p),
        vec!["+refs/heads/main:refs/prefetch/base/main".to_string()]
    );
}
#[test]
fn creation_with_no_base_refs_fetches_nothing() {
    let a = action_for(
        &zero_oid(ObjectFormat::Sha1),
        &"b".repeat(40),
        "refs/heads/agent/x",
    );
    let mut p = policy();
    p.creation_base_refs = vec![];
    assert!(prefetch_specs(&a, &p).is_empty());
}
#[test]
fn concurrent_limit_returns_prefetch_rejected() {
    let g = PrefetchGate::new(1);
    let _p = g.acquire().unwrap();
    assert!(matches!(g.acquire(), Err(BrokerError::PrefetchRejected(_))));
}
#[test]
fn permit_drop_releases_the_slot() {
    let g = PrefetchGate::new(1);
    drop(g.acquire().unwrap());
    assert!(g.acquire().is_ok());
}
/// Counts calls; never touches the network.
struct CountingReader(AtomicUsize);
impl RemoteReader for CountingReader {
    fn fetch(&self, _: &PrivateRepo, _: &[String], _: &PrefetchBudget) -> Result<(), BrokerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[test]
fn no_specs_means_the_reader_is_not_called() {
    let t = tempfile::tempdir().unwrap();
    let r = CountingReader(AtomicUsize::new(0));
    let a = action_for(
        &zero_oid(ObjectFormat::Sha1),
        &"b".repeat(40),
        "refs/heads/agent/x",
    );
    let mut p = policy();
    p.creation_base_refs = vec![];
    run_prefetch(
        &PrefetchGate::new(1),
        &r,
        &priv_repo(t.path()),
        &a,
        &p,
        &BrokerLimits::default(),
    )
    .unwrap();
    assert_eq!(r.0.load(Ordering::SeqCst), 0);
}
#[test]
fn successful_update_prefetch_makes_old_sha_present() {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    let old = commit(&w, "old");
    let remote = bare_remote(t.path());
    git(
        &w,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    let repo = priv_repo(&t.path().join("p")); // create_dir_all inside create()
    let reader = GitFetchReader {
        remote_url: remote.to_str().unwrap().into(),
        git_bin: git_bin(),
    };
    let a = action_for(&old, &"b".repeat(40), "refs/heads/agent/x");
    run_prefetch(
        &PrefetchGate::new(1),
        &reader,
        &repo,
        &a,
        &policy(),
        &BrokerLimits::default(),
    )
    .unwrap();
    let out = repo
        .runner()
        .run(&["cat-file", "-e", &old], Duration::from_secs(10))
        .unwrap();
    assert_eq!(out.code, 0);
}
#[test]
fn byte_budget_is_enforced() {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    std::fs::write(
        w.join("blob"),
        (0..2_000_000u32)
            .flat_map(|i| i.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    git(&w, &["add", "."]);
    let old = commit(&w, "big");
    let remote = bare_remote(t.path());
    git(
        &w,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:refs/heads/agent/x"),
        ],
    );
    let reader = GitFetchReader {
        remote_url: remote.to_str().unwrap().into(),
        git_bin: git_bin(),
    };
    let l = BrokerLimits {
        max_prefetch_bytes: 1024,
        ..Default::default()
    };
    let a = action_for(&old, &"b".repeat(40), "refs/heads/agent/x");
    let e = run_prefetch(
        &PrefetchGate::new(1),
        &reader,
        &priv_repo(&t.path().join("p")),
        &a,
        &policy(),
        &l,
    );
    assert!(
        matches!(&e, Err(BrokerError::PrefetchRejected(m)) if m == "byte budget"),
        "{e:?}"
    );
}
#[test]
fn wall_clock_budget_kills_a_slow_fetch() {
    // A reader that honours the budget the way GitFetchReader does: runs a command under the
    // budget's wall limit. `sleep` stands in for a stalled remote.
    struct Slow;
    impl RemoteReader for Slow {
        fn fetch(
            &self,
            repo: &PrivateRepo,
            _: &[String],
            b: &PrefetchBudget,
        ) -> Result<(), BrokerError> {
            let mut c = std::process::Command::new("/bin/sleep");
            c.arg("30");
            mur_git_broker::git::run_with_timeout(c, b.wall)
                .map(|_| ())
                .map_err(|_| BrokerError::PrefetchRejected("wall".into()))?;
            let _ = repo;
            Ok(())
        }
    }
    let t = tempfile::tempdir().unwrap();
    let l = BrokerLimits {
        max_prefetch_wall_secs: 1,
        ..Default::default()
    };
    let a = action_for(&"a".repeat(40), &"b".repeat(40), "refs/heads/agent/x");
    let start = std::time::Instant::now();
    let e = run_prefetch(
        &PrefetchGate::new(1),
        &Slow,
        &priv_repo(t.path()),
        &a,
        &policy(),
        &l,
    );
    assert!(matches!(e, Err(BrokerError::PrefetchRejected(_))));
    assert!(start.elapsed() < Duration::from_secs(5));
}
#[test]
fn disk_budget_is_enforced_after_fetch() {
    struct Fat;
    impl RemoteReader for Fat {
        fn fetch(
            &self,
            r: &PrivateRepo,
            _: &[String],
            _: &PrefetchBudget,
        ) -> Result<(), BrokerError> {
            std::fs::write(r.path().join("objects/pack/junk"), vec![0u8; 4096]).unwrap();
            Ok(())
        }
    }
    let t = tempfile::tempdir().unwrap();
    let l = BrokerLimits {
        max_prefetch_disk_bytes: 1024,
        ..Default::default()
    };
    let a = action_for(&"a".repeat(40), &"b".repeat(40), "refs/heads/agent/x");
    let e = run_prefetch(
        &PrefetchGate::new(1),
        &Fat,
        &priv_repo(t.path()),
        &a,
        &policy(),
        &l,
    );
    assert!(matches!(e, Err(BrokerError::PrefetchRejected(_))));
}
