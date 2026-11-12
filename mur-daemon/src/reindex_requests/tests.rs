//! Spec §Testing, daemon cases. The worker is a stub shell script standing
//! in for `mur`: it records its argv and pid, then behaves as told by a
//! `mode` file (done / error / crash / hang). Unix-only because of the stub.
#![cfg(unix)]

use super::exec::{self, ExecEnv, Outcome, meta_path};
use super::reconcile::{self, Job};
use super::state::{ReindexState, backoff_secs};
use super::{Scheduler, intake};
use git2::{Repository, Signature};
use mur_common::config::CodebaseReindexConfig;
use mur_common::reindex_request::{REINDEX_REQUEST_DIR, TMP_PREFIX};
use mur_core::codebase::{CodebaseIndex, IndexLock, IndexMetadata};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::watch;

const STUB: &str = r#"#!/bin/sh
# args: project index-worker <name> <root> --no-notify --no-hook
here="$(dirname "$0")"
printf '%s\n' "$@" > "$here/argv"
echo $$ > "$here/pid"
mode="$(cat "$here/mode" 2>/dev/null || echo done)"
prog="$MUR_HOME/indexes/codebase/$3.progress.json"
case "$mode" in
  done)  printf '{"status":"done","total_chunks":1,"done_chunks":1,"errors":0,"started_at":"","finished_at":null,"error_message":null}' > "$prog" ;;
  error) printf '{"status":"error","total_chunks":0,"done_chunks":0,"errors":1,"started_at":"","finished_at":null,"error_message":"boom"}' > "$prog" ;;
  crash) exit 3 ;;
  hang)  exec sleep 30 ;;
esac
"#;

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    bin_dir: PathBuf,
    repos: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let home = root.join("mur");
        let bin_dir = root.join("bin");
        let repos = root.join("repos");
        for d in [&home, &bin_dir, &repos] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(home.join("indexes/codebase")).unwrap();
        let stub = bin_dir.join("mur");
        std::fs::write(&stub, STUB).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _t: t,
            home,
            bin_dir,
            repos,
        }
    }
    fn indexes(&self) -> PathBuf {
        self.home.join("indexes/codebase")
    }
    fn inbox(&self) -> PathBuf {
        self.home.join(REINDEX_REQUEST_DIR)
    }
    fn env(&self, timeout: Duration) -> ExecEnv {
        ExecEnv {
            mur_home: self.home.clone(),
            indexes_dir: self.indexes(),
            mur_bin: self.bin_dir.join("mur"),
            timeout,
        }
    }
    fn mode(&self, m: &str) {
        std::fs::write(self.bin_dir.join("mode"), m).unwrap();
    }
    fn ran(&self) -> bool {
        self.bin_dir.join("argv").exists()
    }
    fn scheduler(&self, cfg: CodebaseReindexConfig) -> Scheduler {
        Scheduler::new(self.env(Duration::from_secs(10)), cfg)
    }
    fn meta(&self, name: &str, root: &Path, head: Option<String>) {
        let m = IndexMetadata {
            project_path: root.display().to_string(),
            indexed_head: head,
            ..IndexMetadata::default()
        };
        std::fs::write(
            meta_path(&self.indexes(), name),
            serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
    }
    fn lock_pid(&self, name: &str) -> Option<u32> {
        let d = std::fs::read_to_string(self.indexes().join(format!("{name}.lock"))).ok()?;
        Some(serde_json::from_str::<IndexLock>(&d).unwrap().pid)
    }
}

fn commit(repo: &Repository, msg: &str) -> String {
    let sig = Signature::now("t", "t@example.com").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .unwrap()
        .to_string()
}

fn repo(at: &Path) -> (Repository, String) {
    let r = Repository::init(at).unwrap();
    let head = commit(&r, "first");
    (r, head)
}

fn cfg() -> CodebaseReindexConfig {
    CodebaseReindexConfig::default()
}

fn job(name: &str, root: &Path) -> Job {
    Job {
        name: name.into(),
        root: root.to_path_buf(),
    }
}

fn no_shutdown() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    std::mem::forget(tx);
    rx
}

// ── intake ──────────────────────────────────────────────────────────────

#[test]
fn wakeup_consumed_on_intake_and_dir_left_empty() {
    let fx = Fx::new();
    mur_common::reindex_request::drop_wakeup(&fx.home).unwrap();
    mur_common::reindex_request::drop_wakeup(&fx.home).unwrap();
    let s = intake::sweep(&fx.inbox(), 256, SystemTime::now());
    assert_eq!(s.wakeups, 2);
    assert_eq!(std::fs::read_dir(fx.inbox()).unwrap().count(), 0);
}

#[test]
fn fifo_symlink_oversized_and_dir_entries_unlinked_without_counting() {
    let fx = Fx::new();
    let inbox = fx.inbox();
    std::fs::create_dir_all(&inbox).unwrap();
    let fifo = inbox.join("fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path; mkfifo has no other preconditions.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let target = fx.repos.join("secret");
    std::fs::write(&target, "do not touch").unwrap();
    std::os::unix::fs::symlink(&target, inbox.join("link")).unwrap();
    std::fs::write(inbox.join("big"), "payload").unwrap();
    std::fs::create_dir(inbox.join("dir")).unwrap();

    // A FIFO that was opened would block this sweep forever.
    let s = intake::sweep(&inbox, 256, SystemTime::now());
    assert_eq!(s.wakeups, 0, "none of these is a wake-up");
    assert_eq!(std::fs::read_dir(&inbox).unwrap().count(), 0);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "do not touch");
}

#[test]
fn intake_is_bounded_per_sweep() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.inbox()).unwrap();
    for i in 0..10 {
        std::fs::write(fx.inbox().join(format!("w{i}")), b"").unwrap();
    }
    let s = intake::sweep(&fx.inbox(), 4, SystemTime::now());
    assert_eq!((s.examined, s.wakeups), (4, 4));
    assert_eq!(std::fs::read_dir(fx.inbox()).unwrap().count(), 6);
}

#[test]
fn in_flight_temp_left_alone_stale_temp_removed() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.inbox()).unwrap();
    let tmp = fx.inbox().join(format!("{TMP_PREFIX}x"));
    std::fs::write(&tmp, b"").unwrap();
    let s = intake::sweep(&fx.inbox(), 256, SystemTime::now());
    assert_eq!(s.wakeups, 0);
    assert!(tmp.exists(), "a writer may be mid-rename");
    let later = SystemTime::now() + intake::TMP_STALE_AFTER + Duration::from_secs(1);
    intake::sweep(&fx.inbox(), 256, later);
    assert!(!tmp.exists(), "abandoned temp is reclaimed");
}

// ── reconcile ───────────────────────────────────────────────────────────

#[test]
fn stale_project_rebuilt_fresh_one_not() {
    let fx = Fx::new();
    let (_a, head_a) = repo(&fx.repos.join("a"));
    let (rb, head_b) = repo(&fx.repos.join("b"));
    fx.meta("a", &fx.repos.join("a"), Some(head_a));
    fx.meta("b", &fx.repos.join("b"), Some(head_b));
    assert!(reconcile::reconcile(&fx.indexes()).stale.is_empty());
    commit(&rb, "agent commit");
    let r = reconcile::reconcile(&fx.indexes());
    assert_eq!(r.stale, vec![job("b", &fx.repos.join("b"))]);
}

#[test]
fn repo_without_meta_is_never_indexed_and_no_index_created() {
    let fx = Fx::new();
    repo(&fx.repos.join("unindexed"));
    let mut s = fx.scheduler(cfg());
    mur_common::reindex_request::drop_wakeup(&fx.home).unwrap();
    assert!(s.plan(0, Instant::now(), SystemTime::now()).is_empty());
    assert_eq!(
        std::fs::read_dir(fx.indexes()).unwrap().count(),
        0,
        "daemon never creates a meta"
    );
}

#[test]
fn symlinked_meta_rejected_torn_meta_deferred() {
    let fx = Fx::new();
    let (r, _) = repo(&fx.repos.join("a"));
    commit(&r, "second");
    let elsewhere = fx.repos.join("planted.meta.json");
    let m = IndexMetadata {
        project_path: fx.repos.join("a").display().to_string(),
        ..Default::default()
    };
    std::fs::write(&elsewhere, serde_json::to_string(&m).unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, meta_path(&fx.indexes(), "a")).unwrap();
    std::fs::write(meta_path(&fx.indexes(), "torn"), "{\"project_pa").unwrap();

    let r = reconcile::reconcile(&fx.indexes());
    assert!(r.stale.is_empty(), "symlinked meta must not yield a job");
    assert!(r.deferred, "torn meta is retried, not dropped");

    let mut s = fx.scheduler(cfg());
    s.plan(0, Instant::now(), SystemTime::now());
    assert!(s.pending, "deferral keeps the next sweep reconciling");
}

#[test]
fn worktree_commit_rebuilds_only_the_worktree_index() {
    let fx = Fx::new();
    let main = fx.repos.join("main");
    let (r, head) = repo(&main);
    let wt_path = fx.repos.join("wt");
    let wt = r.worktree("wt", &wt_path, None).unwrap();
    let wt_repo = Repository::open_from_worktree(&wt).unwrap();
    fx.meta("main", &main, Some(head.clone()));
    fx.meta("wt", &wt_path, Some(head));
    commit(&wt_repo, "in the worktree");
    let stale = reconcile::reconcile(&fx.indexes()).stale;
    assert_eq!(stale, vec![job("wt", &wt_path)], "primary untouched");
}

#[test]
fn worktree_without_its_own_index_rebuilds_nothing() {
    let fx = Fx::new();
    let main = fx.repos.join("main");
    let (r, head) = repo(&main);
    let wt_path = fx.repos.join("wt");
    let wt = r.worktree("wt", &wt_path, None).unwrap();
    fx.meta("main", &main, Some(head));
    commit(
        &Repository::open_from_worktree(&wt).unwrap(),
        "in the worktree",
    );
    assert!(
        reconcile::reconcile(&fx.indexes()).stale.is_empty(),
        "no fallback to primary"
    );
}

#[test]
fn legacy_meta_without_head_is_stale_until_stamped() {
    let fx = Fx::new();
    let (_r, head) = repo(&fx.repos.join("a"));
    fx.meta("a", &fx.repos.join("a"), None);
    assert_eq!(reconcile::reconcile(&fx.indexes()).stale.len(), 1);
    fx.meta("a", &fx.repos.join("a"), Some(head));
    assert!(reconcile::reconcile(&fx.indexes()).stale.is_empty());
}

// ── exec ────────────────────────────────────────────────────────────────

#[test]
fn spawned_argv_carries_both_opt_outs() {
    let args = exec::worker_args(&job("p", Path::new("/r")));
    let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "project",
            "index-worker",
            "p",
            "/r",
            "--no-notify",
            "--no-hook"
        ]
    );
}

#[tokio::test]
async fn successful_worker_runs_with_opt_outs_and_releases_lock() {
    let fx = Fx::new();
    let root = fx.repos.join("a");
    let out = exec::run(
        &fx.env(Duration::from_secs(10)),
        &job("a", &root),
        no_shutdown(),
    )
    .await;
    assert_eq!(out, Outcome::Success);
    let argv = std::fs::read_to_string(fx.bin_dir.join("argv")).unwrap();
    assert!(argv.lines().any(|l| l == "--no-hook") && argv.lines().any(|l| l == "--no-notify"));
    assert_eq!(fx.lock_pid("a"), None, "lock released");
}

#[tokio::test]
async fn held_lock_defers() {
    let fx = Fx::new();
    let root = fx.repos.join("a");
    let idx = CodebaseIndex::in_dir(&fx.indexes(), "a", &root);
    assert!(
        idx.try_acquire_lock().unwrap(),
        "this test process holds it, alive"
    );
    let out = exec::run(
        &fx.env(Duration::from_secs(10)),
        &job("a", &root),
        no_shutdown(),
    )
    .await;
    assert_eq!(out, Outcome::Busy);
    assert!(!fx.ran());
    assert_eq!(
        fx.lock_pid("a"),
        Some(std::process::id()),
        "holder's lock untouched"
    );
}

#[tokio::test]
async fn crashed_child_does_not_leave_project_locked() {
    let fx = Fx::new();
    fx.mode("crash");
    let root = fx.repos.join("a");
    let out = exec::run(
        &fx.env(Duration::from_secs(10)),
        &job("a", &root),
        no_shutdown(),
    )
    .await;
    assert!(matches!(out, Outcome::Failure(_)), "{out:?}");
    assert_eq!(fx.lock_pid("a"), None);
    // The next attempt can take the lock.
    let idx = CodebaseIndex::in_dir(&fx.indexes(), "a", &root);
    assert!(idx.try_acquire_lock().unwrap());
}

#[tokio::test]
async fn lock_names_the_child_while_it_runs() {
    let fx = Fx::new();
    fx.mode("hang");
    let root = fx.repos.join("a");
    let (tx, rx) = watch::channel(false);
    let env = fx.env(Duration::from_secs(20));
    let j = job("a", &root);
    let task = tokio::spawn(async move { exec::run(&env, &j, rx).await });
    let pid_file = fx.bin_dir.join("pid");
    for _ in 0..200 {
        if pid_file.exists() && fx.lock_pid("a") != Some(std::process::id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let child: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        fx.lock_pid("a"),
        Some(child),
        "lock rewritten to the child's pid"
    );

    tx.send(true).unwrap();
    assert_eq!(task.await.unwrap(), Outcome::Cancelled);
    assert_eq!(fx.lock_pid("a"), None, "shutdown releases the lock it owns");
    assert!(
        !mur_common::lock_file::pid_alive(child),
        "shutdown kills the child"
    );
}

#[tokio::test]
async fn timeout_kills_child_and_releases_lock() {
    let fx = Fx::new();
    fx.mode("hang");
    let root = fx.repos.join("a");
    let out = exec::run(
        &fx.env(Duration::from_millis(300)),
        &job("a", &root),
        no_shutdown(),
    )
    .await;
    assert!(
        matches!(&out, Outcome::Failure(m) if m.contains("timed out")),
        "{out:?}"
    );
    assert_eq!(fx.lock_pid("a"), None);
    let child: u32 = std::fs::read_to_string(fx.bin_dir.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!mur_common::lock_file::pid_alive(child));
}

// ── scheduling, backoff ─────────────────────────────────────────────────

#[test]
fn backoff_doubles_from_min_and_caps() {
    assert_eq!(backoff_secs(1, 120, 3600), 120);
    assert_eq!(backoff_secs(2, 120, 3600), 240);
    assert_eq!(backoff_secs(5, 120, 3600), 1920);
    assert_eq!(backoff_secs(6, 120, 3600), 3600);
    assert_eq!(backoff_secs(200, 120, 3600), 3600);
}

#[test]
fn worker_error_backs_off_and_backoff_survives_restart() {
    let fx = Fx::new();
    let (r, _) = repo(&fx.repos.join("a"));
    fx.meta("a", &fx.repos.join("a"), Some("old".into()));
    commit(&r, "x");
    let mut s = fx.scheduler(cfg());
    let t0 = 1_000_000;
    assert_eq!(s.plan(t0, Instant::now(), SystemTime::now()).len(), 1);
    s.finish("a", &Outcome::Failure("boom".into()), t0);

    // A fresh scheduler (daemon restart) still honours the backoff…
    let mut s = fx.scheduler(cfg());
    let soon = t0 + i64::try_from(cfg().min_interval_secs).unwrap() - 1;
    assert!(s.plan(soon, Instant::now(), SystemTime::now()).is_empty());
    // …and the second failure doubles it.
    let after = t0 + i64::try_from(cfg().min_interval_secs).unwrap();
    assert_eq!(s.plan(after, Instant::now(), SystemTime::now()).len(), 1);
    s.finish("a", &Outcome::Failure("boom".into()), after);
    let st = ReindexState::load(&fx.home.join(super::STATE_FILE));
    let p = st.get("a").unwrap();
    assert_eq!(p.failures, 2);
    assert_eq!(
        p.retry_after - after,
        2 * i64::try_from(cfg().min_interval_secs).unwrap()
    );
}

#[tokio::test]
async fn worker_reporting_error_is_a_failure() {
    let fx = Fx::new();
    fx.mode("error");
    let out = exec::run(
        &fx.env(Duration::from_secs(10)),
        &job("a", &fx.repos.join("a")),
        no_shutdown(),
    )
    .await;
    assert_eq!(out, Outcome::Failure("boom".into()));
}

#[test]
fn concurrency_bound_holds_extra_jobs_for_later() {
    let fx = Fx::new();
    for n in ["a", "b"] {
        let (r, _) = repo(&fx.repos.join(n));
        commit(&r, "x");
        fx.meta(n, &fx.repos.join(n), Some("old".into()));
    }
    let mut s = fx.scheduler(cfg());
    let first = s.plan(0, Instant::now(), SystemTime::now());
    assert_eq!(first.len(), 1, "max_concurrent = 1");
    assert!(
        s.plan(0, Instant::now(), SystemTime::now()).is_empty(),
        "slot still taken"
    );
    s.finish(&first[0].name, &Outcome::Success, 0);
    let second = s.plan(0, Instant::now(), SystemTime::now());
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].name, first[0].name);
}

#[test]
fn quiet_sweep_without_wakeup_skips_reconcile_until_due() {
    let fx = Fx::new();
    let mut s = fx.scheduler(cfg());
    let t = Instant::now();
    s.plan(0, t, SystemTime::now());
    assert!(s.last_reconcile.is_some());
    let (r, _) = repo(&fx.repos.join("a"));
    commit(&r, "x");
    fx.meta("a", &fx.repos.join("a"), Some("old".into()));
    assert!(
        s.plan(0, t, SystemTime::now()).is_empty(),
        "no wake-up, not due"
    );
    mur_common::reindex_request::drop_wakeup(&fx.home).unwrap();
    assert_eq!(
        s.plan(0, t, SystemTime::now()).len(),
        1,
        "wake-up triggers reconcile"
    );
}
