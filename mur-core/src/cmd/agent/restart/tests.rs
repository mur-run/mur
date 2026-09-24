use super::*;
use mur_common::{LockFile, agent::LockTransports};
use std::fs;

fn write_lock(path: &Path, pid: u32, build_sha: &str) {
    let lock = LockFile {
        schema: 1,
        uuid: "test-uuid".to_string(),
        name: "test-agent".to_string(),
        pid,
        ppid: 1,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        binary_version: "0.0.0".to_string(),
        transports: LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: "abc".to_string(),
        capabilities: vec![],
        build_sha: build_sha.to_string(),
        proto_version: 1,
        sandbox: None,
    };
    fs::write(path, serde_json::to_vec(&lock).unwrap()).unwrap();
}

/// The staleness baseline is resolved PER AGENT, not once globally.
///
/// Two agents on the same lock sha, whose own runtime symlinks resolve to
/// different binaries: only the one whose binary actually moved is stale.
/// The old single-string baseline could not express this — it had to call
/// both stale or neither, which is how a `--stale` run reported success
/// while restarting agents straight back onto their old binary.
#[test]
fn stale_baseline_is_resolved_per_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    for name in ["keg", "devtree"] {
        let dir = home.join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        write_lock(&dir.join("running.lock"), 1, "oldsha000000");
    }

    // 'keg' points at an upgraded binary; 'devtree' still resolves to the
    // very binary it is already running.
    let per_agent = |agent: &str| match agent {
        "keg" => "newsha111111".to_string(),
        _ => "oldsha000000".to_string(),
    };
    let targets = select_targets_with_on_disk(home, &[], false, true, &per_agent).unwrap();
    assert_eq!(targets, vec!["keg".to_string()]);

    // Negative control: with the OLD global baseline (one sha for all),
    // 'devtree' is dragged in as a false positive.
    let global = |_: &str| "newsha111111".to_string();
    let targets = select_targets_with_on_disk(home, &[], false, true, &global).unwrap();
    assert_eq!(targets, vec!["devtree".to_string(), "keg".to_string()]);
}

/// A bulk selector never looks at an agent without a `running.lock`, so
/// those names have to come out somewhere — and one with a service
/// descriptor is a different, louder problem than one you stopped.
#[test]
fn unexamined_separates_stopped_from_should_be_running() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    for name in ["live", "stopped", "supervised"] {
        fs::create_dir_all(agents.join(name)).unwrap();
        fs::write(agents.join(name).join("profile.yaml"), "name: x\n").unwrap();
    }
    write_lock(&agents.join("live").join("running.lock"), 1, "sha");
    // A non-agent directory must not be reported as a stopped agent.
    fs::create_dir_all(agents.join(".git")).unwrap();

    let (stopped, should_be_running) = unexamined(&agents, &|n| n == "supervised");
    assert_eq!(stopped, vec!["stopped".to_string()]);
    assert_eq!(should_be_running, vec!["supervised".to_string()]);
}

/// Fix 1: bare `mur agent restart` (no name, no --all, no --stale) must
/// return an error — never silently enumerate all running agents.
#[test]
fn select_targets_no_args_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let agents = home.join("agents");

    // Even with a running agent present, no-args must bail.
    let alpha_dir = agents.join("alpha");
    fs::create_dir_all(&alpha_dir).unwrap();
    write_lock(&alpha_dir.join("running.lock"), 1111, "somesha");

    let result = select_targets_with_on_disk(home, &[], false, false, &|_| "somesha".into());
    assert!(result.is_err(), "bare restart with no selector must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("--all") || msg.contains("--stale"),
        "error message should mention --all or --stale, got: {msg}"
    );
}

/// Fix 2: `select_targets_with_on_disk` stale_only branch returns exactly
/// the agent whose build_sha differs from the injected on-disk sha.
#[test]
fn select_targets_stale_only_returns_stale_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let agents = home.join("agents");

    // Agent "alpha": stale sha
    let alpha_dir = agents.join("alpha");
    fs::create_dir_all(&alpha_dir).unwrap();
    write_lock(&alpha_dir.join("running.lock"), 1111, "oldsha000000");

    // Agent "beta": up-to-date sha (matches injected on_disk)
    let beta_dir = agents.join("beta");
    fs::create_dir_all(&beta_dir).unwrap();
    write_lock(&beta_dir.join("running.lock"), 2222, "cur000000000");

    let result =
        select_targets_with_on_disk(home, &[], false, true, &|_| "cur000000000".into()).unwrap();
    assert_eq!(
        result,
        vec!["alpha"],
        "only the stale agent should be returned"
    );
}

#[test]
fn select_targets_named_not_running_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::create_dir_all(home.join("agents").join("ghost")).unwrap();
    // No running.lock
    let result = select_targets(home, &["ghost"], false, false);
    assert!(result.is_err());
}

#[test]
fn select_targets_multiple_names_both_running() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let a_dir = home.join("agents").join("a");
    let b_dir = home.join("agents").join("b");
    fs::create_dir_all(&a_dir).unwrap();
    fs::create_dir_all(&b_dir).unwrap();
    write_lock(&a_dir.join("running.lock"), 1111, "shaaaaaaaaaa");
    write_lock(&b_dir.join("running.lock"), 2222, "shabbbbbbbb");

    let mut result =
        select_targets_with_on_disk(home, &["a", "b"], false, false, &|_| String::new()).unwrap();
    result.sort();
    assert_eq!(result, vec!["a", "b"]);
}

#[test]
fn select_targets_multiple_names_one_not_running_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let a_dir = home.join("agents").join("a");
    let b_dir = home.join("agents").join("b");
    fs::create_dir_all(&a_dir).unwrap();
    fs::create_dir_all(&b_dir).unwrap();
    write_lock(&a_dir.join("running.lock"), 1111, "shaaaaaaaaaa");
    // No running.lock for 'b'

    let result = select_targets_with_on_disk(home, &["a", "b"], false, false, &|_| String::new());
    assert!(
        result.is_err(),
        "must fail-closed when any name isn't running"
    );
}

#[test]
fn select_targets_names_and_all_mutually_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::create_dir_all(home.join("agents")).unwrap();

    let result = select_targets_with_on_disk(home, &["a"], true, false, &|_| String::new());
    assert!(result.is_err(), "names + --all must be rejected");
}

#[test]
fn select_targets_empty_agents_dir_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::create_dir_all(home.join("agents")).unwrap();
    let result = select_targets(home, &[], true, false).unwrap();
    assert!(result.is_empty());
}

/// The pure exists-helper simply reflects on-disk state for the path
/// it's given — path construction is cfg-gated separately and not
/// exercised here (this test is OS-agnostic).
#[test]
fn service_unit_exists_reflects_disk_state() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("run.mur.agent.test.plist");
    assert!(
        !service_unit_exists(&path).unwrap(),
        "must be false before creation"
    );
    fs::write(&path, b"unit").unwrap();
    assert!(
        service_unit_exists(&path).unwrap(),
        "must be true once the file exists"
    );
}

/// A unit path we cannot stat must surface as `Err`, and the restart
/// path must then assume a service rather than direct-respawn beside it.
#[cfg(unix)]
#[test]
fn unreadable_service_dir_is_unknown_and_assumed_installed() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("LaunchAgents");
    fs::create_dir(&dir).unwrap();
    let path = dir.join("run.mur.agent.test.plist");
    fs::write(&path, b"unit").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let probe = service_unit_exists(&path);
    let assumed = has_service_or_assume("test", Some(&path));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    // root ignores mode bits; nothing to prove there.
    if probe.as_ref().is_ok_and(|found| *found) {
        return;
    }
    assert!(probe.is_err(), "EACCES must not read as absent: {probe:?}");
    assert!(assumed, "unknown must be treated as service-managed");
}

#[test]
fn no_resolvable_unit_path_means_no_service() {
    assert!(!has_service_or_assume("test", None));
}

/// Fix 1: SIGKILL-fallback wait must be derived from stop_timeout_secs so
/// it always outlasts the runtime's cooperative drain bound.
#[test]
fn kill_wait_secs_exceeds_stop_timeout() {
    // Default: 15 s drain + 5 s grace = 20 s
    assert_eq!(kill_wait_secs(15), 20);
    // Raised: 60 s drain + 5 s grace = 65 s  (never truncated to 30)
    assert_eq!(kill_wait_secs(60), 65);
    // Grace is always exactly RESTART_KILL_GRACE_SECS
    assert_eq!(kill_wait_secs(0), RESTART_KILL_GRACE_SECS);
    // Result always strictly exceeds the drain bound
    for t in [1u64, 15, 30, 60, 120] {
        assert!(kill_wait_secs(t) > t, "kill wait must exceed drain bound");
    }
}

// ── Attestation mount tests ──────────────────────────────────────────

/// `direct_respawn` refuses to spawn when the runtime target cannot be
/// resolved (the attestation mount canonicalizes before verifying).
/// Negative control: the error comes from the attestation mount on the
/// spawn path, proving the verify call exists.
#[test]
fn direct_respawn_refuses_unresolvable_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().join("agents").join("test-agent");
    std::fs::create_dir_all(&agent_home).unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_AGENT_RUNTIME_BIN", "/nonexistent/mur_agent_nope");
    let err = direct_respawn("test-agent", &agent_home).unwrap_err();
    envg.unset_var("MUR_AGENT_RUNTIME_BIN");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("resolve") && msg.contains("mur_agent_nope"),
        "expected resolution error from attestation mount, got: {msg}"
    );
}

/// `kickstart_service` (macOS path) refuses to kick when the symlink
/// points at a non-resolvable target. This is a structural test: the
/// function now returns `Result<bool>` and the `Err` variant means
/// "attestation failed — never kick."
#[test]
#[cfg(target_os = "macos")]
fn kickstart_service_refuses_unresolvable_symlink() {
    // Redirect bin_dir to a tmp dir with a broken symlink.
    let tmp = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_AGENT_BIN_DIR", tmp.path());
    // No mur_agent_test-kick symlink → canonicalize fails → Err.
    let result = kickstart_service("test-kick");
    envg.unset_var("MUR_AGENT_BIN_DIR");
    match result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("resolve") || msg.contains("attestation"),
                "expected attestation error, got: {msg}"
            );
        }
        Ok(false) => {} // No symlink, no service unit — the kick command just
        // wasn't found. On macOS without a loaded unit, this is
        // expected (the key assertion is that we didn't panic).
        Ok(true) => panic!("kick should not succeed without a unit loaded"),
    }
}

#[test]
fn a_restart_that_lands_on_the_same_binary_is_not_a_success() {
    // The exact shape of the reported bug: --stale picked the agent because
    // on-disk moved to `new`, but the respawn relaunched `old`.
    assert!(restart_changed_nothing("old", "new", "old"));
}

#[test]
fn restarting_an_already_current_agent_is_still_a_success() {
    // Negative control. Without this, "landed == old" alone would call every
    // ordinary restart of an up-to-date agent a failure.
    assert!(!restart_changed_nothing("same", "same", "same"));
}

#[test]
fn a_restart_that_moved_to_the_new_binary_is_a_success() {
    assert!(!restart_changed_nothing("old", "new", "new"));
}

#[test]
fn unknown_shas_never_manufacture_a_failure() {
    // We cannot tell, so we do not accuse.
    assert!(!restart_changed_nothing("unknown", "new", "unknown"));
    assert!(!restart_changed_nothing("old", "unknown", "old"));
    assert!(!restart_changed_nothing("", "new", ""));
}
