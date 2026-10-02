/// #1105: handed nothing dropped, this used to return a failed check with
/// an empty explanation — the blank that #1095 removed from schedules,
/// reintroduced two hours later in the fix for #1091. It was unreachable
/// only because the caller guarded it, and nothing in the signature said so.
#[test]
fn nothing_dropped_is_a_pass_with_its_reason_not_a_blank_failure() {
    for landlock in [true, false] {
        let (ok, detail) = grant_scope_verdict(&[], &[], (3, 2), landlock);
        assert!(ok, "landlock={landlock}: {detail}");
        assert!(!detail.is_empty(), "a verdict must arrive with its reason");
        assert!(detail.contains("3 write"), "{detail}");
        assert!(detail.contains("2 read"), "{detail}");
    }
}

fn pb(p: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(p)
}

/// Landlock cannot carve a grant, so the whole thing is discarded and the
/// user must narrow it.
#[test]
fn under_landlock_an_overlapping_write_grant_is_reported_as_lost() {
    let (ok, detail) = grant_scope_verdict(&[pb("/home/d/.mur")], &[], (1, 0), true);
    assert!(!ok, "{detail}");
    assert!(detail.contains("DROPPED WHOLE"), "{detail}");
    assert!(detail.contains("specific subdirectory"), "{detail}");
}

/// The bug this replaces: the same sentence fired on macOS, where the grant
/// IS installed and only the launch-chain paths inside it are re-denied.
/// Telling the user it was dropped sends them to narrow a working grant.
#[test]
fn without_landlock_the_same_grant_is_installed_and_not_called_lost() {
    let (ok, detail) = grant_scope_verdict(&[pb("/Users/d/.mur")], &[], (1, 0), false);
    assert!(ok, "an installed grant is not a failure: {detail}");
    assert!(
        !detail.contains("DROPPED WHOLE"),
        "must not claim a loss of access that did not happen: {detail}"
    );
    // Still named — the grant covers less than it reads as, on both.
    assert!(detail.contains("launch-chain path"), "{detail}");
}

/// The read half IS dropped whole everywhere, so it fails on both.
#[test]
fn a_read_overlap_fails_on_either_platform() {
    for landlock in [true, false] {
        let (ok, detail) = grant_scope_verdict(&[], &[pb("/x/.mur")], (0, 1), landlock);
        assert!(!ok, "landlock={landlock}: {detail}");
        assert!(detail.contains("DROPPED WHOLE"), "{detail}");
    }
}

use super::*;

#[test]
fn pkg_format_yields_minimal_checks() {
    let results = checks_for("pkg");
    assert!(results.iter().any(|r| r.name == "cargo"));
    assert!(!results.iter().any(|r| r.name == "node"));
}

#[test]
fn gui_format_demands_node_and_npm() {
    let results = checks_for("gui");
    assert!(results.iter().any(|r| r.name == "node"));
    assert!(results.iter().any(|r| r.name == "npm"));
    assert!(results.iter().any(|r| r.name == "tauri-cli"));
}

#[test]
fn host_target_is_detected_or_skipped() {
    let results = checks_for("all");
    let arch = results
        .iter()
        .find(|r| r.name == "host-target")
        .expect("host-target check should always run for `all`");
    assert!(matches!(
        arch.status,
        CheckStatus::Ok | CheckStatus::Skipped
    ));
}

/// `SandboxPolicy::dropped_grants` recorded this from the day the launch
/// chain landed, with a comment saying it was "recorded for the
/// runtime-doctor". Nothing ever read it. A grant whose SCOPE overlaps the
/// launch chain is dropped WHOLE — Landlock has no deny rule to carve one
/// out — so an agent could lose an entire grant and learn about it from an
/// errno.
#[test]
fn a_grant_that_swallows_the_launch_chain_is_reported() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("alice");
    std::fs::create_dir_all(&agent_home).unwrap();

    let fs = mur_common::agent::FilesystemEntitlement {
        read: vec![],
        // Contains <mur_home>/agents, which is launch-chain protected.
        write: vec![mur_home.display().to_string()],
        deny: vec![],
    };
    let c = check_dropped_launch_chain_grants(&fs, &agent_home);
    // Named on every platform — the user has to know the grant is not doing
    // what it reads as.
    assert!(c.detail.contains("launch-chain path"), "{}", c.detail);
    // The verdict differs because the kernels do. Landlock cannot carve, so
    // Linux discards the grant; macOS installs it and re-denies the
    // protected paths inside it, which is not a failure and must not be
    // reported as one.
    if cfg!(target_os = "linux") {
        assert!(!c.ok, "Linux discards the grant: {}", c.detail);
        assert!(c.detail.contains("DROPPED WHOLE"), "{}", c.detail);
    } else {
        assert!(c.ok, "the grant IS installed here: {}", c.detail);
        assert!(
            !c.detail.contains("DROPPED WHOLE"),
            "must not claim a loss of access that did not happen: {}",
            c.detail
        );
    }
}

/// The read side of the same check. A read grant reaching the credential
/// store is dropped by the sandbox exactly as an overbroad write grant is,
/// and the user needs to be told — before #850 nothing reported it, and on
/// Linux nothing stopped it either.
#[test]
fn a_read_grant_that_reaches_the_credential_store_is_reported() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("alice");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(mur_home.join("secrets")).unwrap();

    let fs = mur_common::agent::FilesystemEntitlement {
        // Contains <mur_home>/secrets.
        read: vec![mur_home.display().to_string()],
        write: vec![],
        deny: vec![],
    };
    let c = check_dropped_launch_chain_grants(&fs, &agent_home);
    assert!(!c.ok, "an overbroad read grant must fail: {}", c.detail);
    assert!(c.detail.contains("read grant"), "{}", c.detail);
    assert!(c.detail.contains("DROPPED WHOLE"), "{}", c.detail);
}

/// A grant that does not reach the launch chain is installed as written,
/// and must not be reported — a check that fires on a correct setup is a
/// check people stop reading.
#[test]
fn an_ordinary_project_grant_is_not_reported() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("alice");
    std::fs::create_dir_all(&agent_home).unwrap();
    let proj = tmp.path().join("code").join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    let fs = mur_common::agent::FilesystemEntitlement {
        read: vec![],
        write: vec![proj.display().to_string()],
        deny: vec![],
    };
    let c = check_dropped_launch_chain_grants(&fs, &agent_home);
    assert!(c.ok, "an ordinary grant was reported: {}", c.detail);
}

/// The check that used to be `Check::new("entitlements", true, "parsed")`
/// — unconditionally true, never looking at a path. A grant the kernel
/// will drop must make it FALSE, and must name the path, because the only
/// other signal the user gets is an unrelated errno much later.
#[test]
fn entitlements_check_names_a_grant_the_kernel_will_drop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let present = tmp.path().join("exists");
    std::fs::create_dir_all(&present).unwrap();
    let absent = tmp.path().join("not-there");

    let fs = mur_common::agent::FilesystemEntitlement {
        read: vec![present.display().to_string()],
        write: vec![absent.display().to_string()],
        deny: vec![],
    };
    let c = check_filesystem_entitlements(&fs);

    assert!(!c.ok, "a droppable grant must fail the check: {}", c.detail);
    assert!(
        c.detail.contains("not-there"),
        "the missing path must be named: {}",
        c.detail
    );
    assert!(
        !c.detail.contains("exists"),
        "a grant that IS present must not be reported as dropped: {}",
        c.detail
    );
}

/// All grants present is the ordinary case and must stay green — and the
/// detail must say how many were actually looked at, so "ok" is not the
/// same message whether it checked five paths or none.
#[test]
fn entitlements_check_passes_when_every_granted_path_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();

    let fs = mur_common::agent::FilesystemEntitlement {
        read: vec![a.display().to_string()],
        write: vec![b.display().to_string()],
        // A deny for a path that does not exist is fine and must be ignored.
        deny: vec![tmp.path().join("never").display().to_string()],
    };
    let c = check_filesystem_entitlements(&fs);

    assert!(c.ok, "all-present grants must pass: {}", c.detail);
    assert!(
        c.detail.contains('2'),
        "the detail must say how many grants were checked: {}",
        c.detail
    );
}

/// A dangling symlink is a path the sandbox cannot grant either: the link
/// exists, its target does not. Only a FOLLOWING stat sees that, which is
/// why this check uses `metadata` and not `symlink_metadata` — and it is
/// also what `perm allow-*` uses, so both halves agree on "exists".
#[cfg(unix)]
#[test]
fn entitlements_check_catches_a_dangling_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let link = tmp.path().join("dangling");
    std::os::unix::fs::symlink(tmp.path().join("no-such-target"), &link).unwrap();

    let fs = mur_common::agent::FilesystemEntitlement {
        read: vec![link.display().to_string()],
        write: vec![],
        deny: vec![],
    };
    let c = check_filesystem_entitlements(&fs);
    assert!(!c.ok, "a dangling symlink grant must fail: {}", c.detail);
}

#[test]
fn agent_doctor_named_checks_model_ref_and_mcp() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let agent_dir = mur_home.join("agents").join("coach");
    std::fs::create_dir_all(&agent_dir).unwrap();

    let mut profile = mur_common::AgentProfile::default_for_tests();
    profile.model_ref = Some("nonexistent_ref".into());
    std::fs::write(
        agent_dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();

    // Isolate resolve_mur_home()/ModelRegistry::default_path() to the
    // temp home; empty models.yaml means "nonexistent_ref" won't resolve.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", &mur_home);
    let report = agent_doctor(&mur_home, "coach").unwrap();
    envg.unset_var("MUR_HOME");

    assert!(
        report.iter().any(|c| c.name == "model_ref" && !c.ok),
        "expected a failing model_ref check, got: {report:?}"
    );
    // The dangling ref also fails model resolution, which at runtime
    // silently becomes an echo stub — doctor must say so.
    assert!(
        report
            .iter()
            .any(|c| c.name == "model" && !c.ok && c.detail.contains("echo stub")),
        "expected a failing model-resolution check naming the echo fallback, got: {report:?}"
    );
}

/// A spawned MCP server inherits the agent's sandbox in full, so the
/// policy is per-agent and not per-server. Doctor has to say so — it is
/// where someone lands after asking which of their servers can reach what.
/// Reported as OK, not a failure: it is a granularity limit the user
/// cannot fix, and a permanent red trains people to ignore the report.
#[test]
fn agent_doctor_states_that_policy_is_per_agent_not_per_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let agent_dir = mur_home.join("agents").join("scoped");
    std::fs::create_dir_all(&agent_dir).unwrap();

    let mut profile = mur_common::AgentProfile::default_for_tests();
    profile.mcp_servers.push(mur_common::agent::McpServerEntry {
        name: "media".into(),
        command: "npx".into(),
        ..Default::default()
    });
    std::fs::write(
        agent_dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();

    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", &mur_home);
    let report = agent_doctor(&mur_home, "scoped").unwrap();
    envg.unset_var("MUR_HOME");

    let c = report
        .iter()
        .find(|c| c.name == "sandbox_scope")
        .expect("expected a sandbox_scope check");
    assert!(c.ok, "a granularity limit is not this agent's fault");
    assert!(
        c.detail.contains("per-agent") && c.detail.contains("narrower"),
        "must name the actual limit — shared scope, not absent scope: {}",
        c.detail
    );
}

/// No MCP servers, nothing spawned, nothing to warn about — the note must
/// not become background noise on every agent.
#[test]
fn agent_doctor_is_silent_about_scope_without_mcp_servers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path().to_path_buf();
    let agent_dir = mur_home.join("agents").join("bare");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let profile = mur_common::AgentProfile::default_for_tests();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();

    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", &mur_home);
    let report = agent_doctor(&mur_home, "bare").unwrap();
    envg.unset_var("MUR_HOME");

    assert!(report.iter().all(|c| c.name != "sandbox_scope"));
}
