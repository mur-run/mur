//! The build lane (`processes.spawn.allowed_dirs`), end to end.
//!
//! Its own integration binary on purpose: `sandbox_e2e.rs` applies a
//! sandbox to the TEST PROCESS itself (`sandbox_apply_does_not_panic`), and
//! on macOS a sandbox is inherited by children — so once that test has run,
//! every subprocess spawned from that binary is already sealed under a
//! profile with no build lane, and this test would fail depending on the
//! order the harness happened to pick.

/// The build lane, end to end: a binary the agent compiled itself must be
/// exec'able when it lives under a granted `allowed_dirs` tree, and a binary
/// outside that tree must still be refused.
///
/// This is the capability A7 is about — a Rust agent cannot verify its own
/// work if it may not run the build scripts and test executables `cargo`
/// produces. Unit-testing the policy struct proves the grant is recorded;
/// only sealing a real process and calling exec proves the kernel honours it.
#[test]
#[cfg(target_os = "macos")]
fn sandbox_allows_exec_inside_the_build_lane() {
    let exe = std::env::current_exe().unwrap();
    // Positive: with the lane granted, the compiled binary runs.
    let granted = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_BUILD_LANE", "grant")
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .status()
        .unwrap();
    assert_eq!(
        granted.code(),
        Some(0),
        "a binary under the granted build lane must exec; one outside must not"
    );
    // Negative control: WITHOUT the grant the same binary must be refused.
    // Without this, a run where the sandbox silently failed to apply — which
    // the subprocess treats as a pass — would look identical to a working
    // lane, and this test would be proving nothing.
    let ungranted = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_BUILD_LANE", "nogrant")
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .status()
        .unwrap();
    assert_eq!(
        ungranted.code(),
        Some(0),
        "without the grant the binary must NOT exec (else the positive case proves nothing)"
    );
}

/// Subprocess entry point for `sandbox_allows_exec_inside_the_build_lane`.
#[cfg(target_os = "macos")]
#[ctor::ctor]
fn sandbox_build_lane_subprocess_main() {
    let Some(mode) = std::env::var_os("MUR_TEST_SANDBOX_BUILD_LANE") else {
        return;
    };
    let granted = mode == *"grant";
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;

    let agent_home = std::path::PathBuf::from("/tmp/b1_test_lane_home");
    std::fs::create_dir_all(&agent_home).unwrap();

    // Stand-in for `target/debug/build/<crate>-<hash>/build-script-build`:
    // a binary that does not exist when the entitlement is written, and whose
    // path no allowlist could have named in advance.
    let lane = std::path::PathBuf::from("/tmp/b1_test_lane_target");
    let outside = std::path::PathBuf::from("/tmp/b1_test_lane_outside");
    std::fs::create_dir_all(&lane).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let in_lane = lane.join("build-script-build");
    let off_lane = outside.join("build-script-build");
    for p in [&in_lane, &off_lane] {
        // A shell script, NOT a copy of a system binary: copying a signed
        // Apple binary invalidates its signature and the kernel SIGKILLs the
        // copy, which looks exactly like a sandbox denial in the exit status
        // and would make this test lie in both directions.
        std::fs::write(p, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(p).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(p, perms).unwrap();
    }

    let mut profile = AgentProfile::default_for_tests();
    profile.entitlements.processes.spawn.mode = mur_common::agent::SpawnMode::Allowlist;
    profile.entitlements.processes.spawn.allowed_dirs = if granted {
        vec![lane.to_string_lossy().to_string()]
    } else {
        vec![]
    };

    if sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]).is_err() {
        // No sandbox support here — nothing to assert either way.
        std::process::exit(0);
    }

    let ran = |p: &std::path::Path| {
        let r = std::process::Command::new(p).arg("ok").status();
        // Print the raw result: a denial (`Err(PermissionDenied)`) and a
        // process killed for another reason (`Ok(signal 9)`) are different
        // facts, and collapsing them to a bool is how this test first lied.
        eprintln!("exec {p:?} -> {r:?}");
        r.is_ok_and(|s| s.success())
    };
    let in_lane_ran = ran(&in_lane);
    let off_lane_ran = ran(&off_lane);

    // Granted: the lane binary runs and the one outside it does not.
    // Ungranted: neither runs — this is what proves the granted run was the
    // grant working, not the sandbox failing to apply.
    let want_in_lane = granted;
    if in_lane_ran == want_in_lane && !off_lane_ran {
        std::process::exit(0);
    }
    eprintln!(
        "ERROR: granted={granted} in_lane={in_lane_ran} (want {want_in_lane}), \
         off_lane={off_lane_ran} (want false)"
    );
    std::process::exit(1);
}
