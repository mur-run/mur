use super::*;

/// Write a `running.lock` naming `pid` and the sentinel that carries the
/// startup flock, and return the agent dir.
fn seed_runtime_state(mur_home: &Path, slug: &str, pid: u32) -> PathBuf {
    let dir = mur_home.join("agents").join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let lock = mur_common::LockFile {
        schema: 1,
        uuid: "u".into(),
        name: slug.into(),
        pid,
        ppid: 0,
        started_at: String::new(),
        binary_version: String::new(),
        transports: mur_common::agent::LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: String::new(),
        capabilities: vec![],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
        startup_override: None,
        override_rejected: None,
    };
    std::fs::write(dir.join("running.lock"), serde_json::to_vec(&lock).unwrap()).unwrap();
    std::fs::write(dir.join("running.sentinel"), b"").unwrap();
    std::fs::write(dir.join("agent.sock"), b"").unwrap();
    dir
}

/// A pid that is guaranteed not to be alive: spawn a child and reap it.
fn dead_pid() -> u32 {
    let mut c = Command::new(if cfg!(windows) { "cmd" } else { "true" })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = c.id();
    let _ = c.wait();
    pid
}

/// A runtime the Hub did not spawn (CLI, launchd, an earlier Hub) has to
/// land in the tracked set, or the Agents page reports it idle while it is
/// plainly running — which is what every agent looked like after a Hub
/// restart, because the set started empty and only `Start` ever filled it.
#[test]
fn adopt_live_runtimes_finds_foreign_runtimes_and_ignores_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    seed_runtime_state(tmp.path(), "alive", std::process::id());
    seed_runtime_state(tmp.path(), "exited", dead_pid());
    std::fs::create_dir_all(tmp.path().join("agents").join("never-started")).unwrap();

    assert_eq!(
        adopt_live_runtimes(tmp.path()),
        HashSet::from(["alive".to_string()])
    );
}

/// No `~/.mur/agents` yet (fresh install) must not panic the supervisor.
#[test]
fn adopt_live_runtimes_on_a_missing_agents_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(adopt_live_runtimes(tmp.path()).is_empty());
}

/// Deleting `running.sentinel` under a live owner destroys the flock that
/// makes supervisor startup mutually exclusive — the next Start then
/// acquires a "lock" nobody holds and a second supervisor runs against the
/// same agent home (#790).
#[test]
fn clear_runtime_state_refuses_while_a_live_process_owns_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "live", std::process::id());

    clear_runtime_state(tmp.path(), "live");

    for f in ["running.lock", "agent.sock", "running.sentinel"] {
        assert!(
            dir.join(f).exists(),
            "{f} must survive: its owner is still running"
        );
    }
}

#[test]
fn clear_runtime_state_removes_state_left_by_a_dead_process() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "dead", dead_pid());

    clear_runtime_state(tmp.path(), "dead");

    for f in ["running.lock", "agent.sock"] {
        assert!(!dir.join(f).exists(), "{f} should have been cleared");
    }
    assert!(
        dir.join("running.sentinel").exists(),
        "the sentinel is never unlinked; only its flock carries ownership"
    );
}

/// A booting supervisor holds the sentinel flock before `running.lock`
/// exists. Clearing in that window deletes the inode it locks — #790.
#[test]
fn clear_runtime_state_refuses_while_the_sentinel_is_flocked() {
    use fs2::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "booting", dead_pid());
    std::fs::remove_file(dir.join("running.lock")).unwrap();
    let held = std::fs::File::open(dir.join("running.sentinel")).unwrap();
    held.try_lock_exclusive().unwrap();

    clear_runtime_state(tmp.path(), "booting");

    assert!(
        dir.join("running.sentinel").exists(),
        "flocked sentinel must survive"
    );
    assert!(dir.join("agent.sock").exists());
    drop(held);
}

/// The flock probe can't see a holder on a filesystem without flock
/// support; the pid the runtime writes into the sentinel is the fallback.
#[test]
fn clear_runtime_state_refuses_when_the_sentinel_names_a_live_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "booting", dead_pid());
    std::fs::remove_file(dir.join("running.lock")).unwrap();
    std::fs::write(dir.join("running.sentinel"), std::process::id().to_string()).unwrap();

    clear_runtime_state(tmp.path(), "booting");

    assert!(dir.join("running.sentinel").exists());
}

#[test]
fn clear_runtime_state_clears_a_sentinel_naming_a_dead_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "dead", dead_pid());
    std::fs::write(dir.join("running.sentinel"), dead_pid().to_string()).unwrap();

    clear_runtime_state(tmp.path(), "dead");

    for f in ["running.lock", "agent.sock"] {
        assert!(!dir.join(f).exists(), "{f} should have been cleared");
    }
    let left = std::fs::read_to_string(dir.join("running.sentinel")).unwrap();
    assert!(
        left.is_empty(),
        "a dead owner's pid is erased, so a reused pid can't revive it: {left:?}"
    );
}

/// The probe used to release its flock and only then unlink the sentinel.
/// A runtime that flocked the file in that gap kept a lock on an inode the
/// next opener couldn't see — #790 again. The probe flock now spans the
/// clear, and the sentinel's inode never changes.
#[cfg(unix)]
#[test]
fn clear_runtime_state_keeps_the_sentinel_inode_a_racer_would_lock() {
    use fs2::FileExt;
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_runtime_state(tmp.path(), "racing", dead_pid());
    let sentinel = dir.join("running.sentinel");
    let before = std::fs::metadata(&sentinel).unwrap().ino();
    // What a booting runtime opens before it tries the flock.
    let racer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sentinel)
        .unwrap();

    clear_runtime_state(tmp.path(), "racing");

    assert_eq!(std::fs::metadata(&sentinel).unwrap().ino(), before);
    racer.try_lock_exclusive().unwrap();
    let late = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&sentinel)
        .unwrap();
    assert!(
        late.try_lock_exclusive().is_err(),
        "a later runtime must meet the racer's flock, not a fresh inode"
    );
}

#[test]
fn clear_runtime_state_on_a_missing_agent_dir_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    clear_runtime_state(tmp.path(), "never-existed");
    assert!(!tmp.path().join("agents").join("never-existed").exists());
}

#[test]
fn lock_owner_pid_reports_only_live_owners() {
    let tmp = tempfile::tempdir().unwrap();
    seed_runtime_state(tmp.path(), "live", std::process::id());
    seed_runtime_state(tmp.path(), "dead", dead_pid());

    assert_eq!(lock_owner_pid(tmp.path(), "live"), Some(std::process::id()));
    assert_eq!(lock_owner_pid(tmp.path(), "dead"), None);
    assert_eq!(lock_owner_pid(tmp.path(), "never-existed"), None);
}

#[test]
fn runtime_state_serializes_correctly() {
    let running = RuntimeState::Running { pid: 1234 };
    let j = serde_json::to_string(&running).unwrap();
    assert!(j.contains("\"state\":\"running\""), "got: {j}");
    assert!(j.contains("1234"));

    let restarting = RuntimeState::Restarting {
        attempt: 2,
        backoff_secs: 4,
    };
    let j2 = serde_json::to_string(&restarting).unwrap();
    assert!(j2.contains("\"state\":\"restarting\""), "got: {j2}");

    let j3 = serde_json::to_string(&RuntimeState::Failed).unwrap();
    assert!(j3.contains("\"state\":\"failed\""), "got: {j3}");
}

#[tokio::test]
async fn supervisor_start_stop_does_not_panic() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let sup = Supervisor::new(dir.path().to_path_buf());
    assert!(sup.status_receiver().borrow().is_empty());

    // With no runtime binary present, start returns Err but the agent is
    // still tracked (it was inserted before the launch attempt).
    let _ = sup.start("ghost").await;
    sup.stop("ghost").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    sup.shutdown().await;
}

#[tokio::test]
async fn multiple_agents_tracked_independently() {
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let sup = Supervisor::new(dir.path().to_path_buf());
    let _ = sup.start("alpha").await;
    let _ = sup.start("beta").await;
    sup.stop("alpha").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    sup.shutdown().await;
}

/// A directory is not an executable runtime: the pre-spawn resolution
/// (find → verify → spawn) must fail with the human-readable error, never
/// panic and never spawn. The signature-verification mount lives in this
/// same fail-before-spawn path; its behavior is covered in mur-common
/// with a test requirement (Task 1) — this test pins the structural guard.
///
/// Env is saved/restored rather than clobbered (tests share one process
/// env). PATH keeps every entry except ones that resolve to a real runtime
/// binary, so the directory override cannot fall through to an installed
/// MUR on PATH — which would spawn a stray runtime instead of failing.
#[test]
fn spawn_runtime_fails_cleanly_on_invalid_target() {
    let dir = std::env::temp_dir().join(format!("mur-sidecar-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prev_path = std::env::var_os("PATH");
    #[cfg(target_os = "windows")]
    let runtime_name = "mur-agent-runtime.exe";
    #[cfg(not(target_os = "windows"))]
    let runtime_name = "mur-agent-runtime";
    let path_without_runtime = prev_path.as_ref().map(|p| {
        std::env::join_paths(
            std::env::split_paths(&p).filter(|d| !is_real_binary(&d.join(runtime_name))),
        )
        .expect("PATH entries join back together")
    });
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_AGENT_RUNTIME_BIN", &dir);
    if let Some(p) = &path_without_runtime {
        envg.set_var("PATH", p);
    }
    let err = spawn_runtime("attest-test", std::path::Path::new("/tmp")).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        err.contains("agent runtime not found"),
        "unexpected error: {err}"
    );
}
