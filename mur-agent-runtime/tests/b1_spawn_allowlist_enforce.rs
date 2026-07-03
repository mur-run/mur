//! End-to-end proof that the SBPL profile generated for
//! `SpawnMode::Allowlist` actually enforces process-exec restrictions at
//! the kernel level via `sandbox-exec`, not just at profile-generation
//! time. This directly reproduces (and closes) the dogfood report that
//! `SPAWN_TOOLS` allowlisting had no OS-level backing.
//!
//! Gated behind `MUR_TEST_SANDBOX=1`, mirroring the `MUR_TEST_SOCKETS`
//! pattern in `mur-core/src/a2a_dial.rs`: `sandbox-exec` refuses to apply
//! a *nested* sandbox profile from inside a process that is itself
//! already sandboxed (observed as `sandbox_apply: Operation not
//! permitted`), which is exactly the situation an AI coding agent's own
//! shell may be running under. The test is fully functional on a normal
//! (non-nested-sandboxed) macOS dev machine or CI runner; set the env var
//! there to run it for real.
//!
//! **System-path exemption (intentional, confirmed empirically):**
//! `MACOS_SYSTEM_EXEC_PATHS` (`/bin`, `/usr/bin`, `/usr/lib`) is re-allowed
//! under the deny-exec baseline so the shell interpreter and coreutils the
//! `bash` tool depends on keep working — see the doc comment on that const
//! in `sandbox/macos.rs`. That means a coreutil like `/bin/mkdir` is *not*
//! denied even though it is not in `spawn_allowed_paths`;
//! `system_path_coreutil_runs_under_enforced_profile` below documents and
//! locks in that exemption. The allowlist instead bounds *non-system*
//! binaries (downloaded, Homebrew, project-local) — the real threat
//! surface — proven by
//! `non_allowlisted_tempdir_binary_is_denied_under_enforced_profile`. A
//! stricter shell-only `SpawnMode::Strict` also fences those system
//! paths (only the resolved shell + spawn_allowed_paths/prefixes remain
//! exec-able); see
//! `strict_mode_denies_system_path_but_allows_shell_and_allowlisted_binary`
//! below.
#![cfg(target_os = "macos")]

use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::macos::build_sbpl_profile;
use mur_common::agent::SpawnMode;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// Bail out early (with an explanatory `eprintln`) unless the operator has
/// opted in via `MUR_TEST_SANDBOX=1`. Returns `true` if the caller should
/// return immediately.
fn skip_unless_opted_in(test_name: &str) -> bool {
    if std::env::var("MUR_TEST_SANDBOX").as_deref() != Ok("1") {
        eprintln!(
            "skipping {test_name}: set MUR_TEST_SANDBOX=1 on a machine that \
             permits `sandbox-exec` to apply a profile (this shell must not \
             itself already be sandboxed — macOS forbids nesting; see module \
             docs for why this is gated)"
        );
        return true;
    }
    false
}

/// Build a policy with exactly one allowed binary (`/usr/bin/true`, which
/// is present on every macOS install and exits 0 with no side effects),
/// generate its SBPL profile via the real slice-2 code path, and write it
/// to a temp file for `sandbox-exec -f`.
fn write_allowlist_profile() -> (tempfile::TempDir, PathBuf) {
    let policy = SandboxPolicy {
        spawn_mode: SpawnMode::Allowlist,
        spawn_allowed_paths: vec![PathBuf::from("/usr/bin/true")],
        ..SandboxPolicy::default()
    };
    let sbpl = build_sbpl_profile(&policy);

    let dir = tempfile::TempDir::new().expect("create temp dir for profile");
    let profile_path = dir.path().join("allowlist.sb");
    let mut f = std::fs::File::create(&profile_path).expect("create profile file");
    f.write_all(sbpl.as_bytes()).expect("write profile file");
    (dir, profile_path)
}

/// Run `sandbox-exec -f <profile> <argv...>` and return whether the child
/// process reported success (exit status 0). `sandbox-exec` itself exits
/// non-zero (and the child never even starts) when the *kernel* denies the
/// exec — that failure mode, not a graceful in-app error, is exactly what
/// this test needs to observe to prove real enforcement.
fn run_under_profile(profile_path: &std::path::Path, argv: &[&str]) -> std::process::ExitStatus {
    Command::new("sandbox-exec")
        .arg("-f")
        .arg(profile_path)
        .args(argv)
        .status()
        .expect("failed to spawn sandbox-exec itself")
}

#[test]
fn allowlisted_binary_execs_successfully_under_enforced_profile() {
    if skip_unless_opted_in("allowlisted_binary_execs_successfully_under_enforced_profile") {
        return;
    }
    let (_dir, profile_path) = write_allowlist_profile();

    let status = run_under_profile(&profile_path, &["/usr/bin/true"]);
    assert!(
        status.success(),
        "the one allowlisted binary (/usr/bin/true) must exec successfully \
         under its own profile, got exit status {status:?}"
    );
}

#[test]
fn non_allowlisted_tempdir_binary_is_denied_under_enforced_profile() {
    if skip_unless_opted_in("non_allowlisted_tempdir_binary_is_denied_under_enforced_profile") {
        return;
    }
    let (_dir, profile_path) = write_allowlist_profile();

    // Copy /usr/bin/true into the tempdir so it's a real executable that is
    // NOT under a MACOS_SYSTEM_EXEC_PATHS root and NOT in spawn_allowed_paths
    // — this is the actual threat surface the allowlist is meant to bound
    // (a downloaded/Homebrew/project-local binary), unlike the /bin, /usr/bin,
    // /usr/lib coreutils which are deliberately exempt (see module docs
    // above and `system_path_coreutil_runs_under_enforced_profile` below).
    let outside_binary = _dir.path().join("not_allowlisted_true");
    std::fs::copy("/usr/bin/true", &outside_binary).expect("copy /usr/bin/true into tempdir");
    let mut perms = std::fs::metadata(&outside_binary)
        .expect("stat copied binary")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&outside_binary, perms).expect("chmod copied binary");

    let status = run_under_profile(&profile_path, &[outside_binary.to_str().unwrap()]);
    assert!(
        !status.success(),
        "a tempdir-local binary outside spawn_allowed_paths and outside \
         MACOS_SYSTEM_EXEC_PATHS must be denied (EPERM/EACCES) by the \
         kernel, got exit status {status:?}"
    );
}

#[test]
fn system_path_coreutil_runs_under_enforced_profile() {
    if skip_unless_opted_in("system_path_coreutil_runs_under_enforced_profile") {
        return;
    }
    let (_dir, profile_path) = write_allowlist_profile();
    let target = _dir.path().join("system_path_exemption_probe");

    // DOCUMENTED SYSTEM-PATH EXEMPTION: /bin/mkdir is not in
    // spawn_allowed_paths (only /usr/bin/true is), yet it still runs
    // because MACOS_SYSTEM_EXEC_PATHS re-allows all of /bin, /usr/bin,
    // /usr/lib under the deny-exec baseline so the shell tool stays
    // usable. This is the intentional v2 semantic, not a gap: the
    // allowlist bounds non-system binaries; a stricter shell-only mode
    // that also fences system paths is a documented follow-up.
    let status = run_under_profile(&profile_path, &["/bin/mkdir", target.to_str().unwrap()]);
    assert!(
        status.success(),
        "coreutils under MACOS_SYSTEM_EXEC_PATHS must remain runnable \
         (documented system-path exemption), got exit status {status:?}"
    );
    assert!(
        target.exists(),
        "mkdir under the system-path exemption must actually have run \
         (not merely exited 0 without executing)"
    );
}

#[test]
fn non_allowlisted_cat_is_denied_under_enforced_profile() {
    if skip_unless_opted_in("non_allowlisted_cat_is_denied_under_enforced_profile") {
        return;
    }
    let (_dir, profile_path) = write_allowlist_profile();

    let status = run_under_profile(&profile_path, &["/bin/cat", "/etc/hostname"]);
    assert!(
        !status.success(),
        "cat is not in the spawn allowlist and must be denied by the \
         kernel, got exit status {status:?}"
    );
}

#[test]
fn non_allowlisted_python3_is_denied_under_enforced_profile() {
    if skip_unless_opted_in("non_allowlisted_python3_is_denied_under_enforced_profile") {
        return;
    }
    let (_dir, profile_path) = write_allowlist_profile();

    let status = run_under_profile(&profile_path, &["/usr/bin/python3", "-c", "pass"]);
    assert!(
        !status.success(),
        "python3 is not in the spawn allowlist and must be denied by the \
         kernel, got exit status {status:?}"
    );
}

/// Build a `SpawnMode::Strict` policy: one tempdir-local allowlisted
/// binary (a copy of `/usr/bin/true`, so it is a real executable that is
/// NOT under any `MACOS_SYSTEM_EXEC_PATHS` root) plus `/bin/bash` itself
/// (mirroring what `SandboxPolicy::from_entitlements` auto-seeds into
/// `spawn_allowed_paths` for `Strict` mode — see `policy.rs`'s
/// `strict_mode_seeds_shell_into_spawn_allowed` unit test). Unlike
/// `write_allowlist_profile`, `MACOS_SYSTEM_EXEC_PATHS` is NOT re-allowed
/// under `Strict`, so a coreutil like `/bin/mkdir` must be kernel-denied.
fn write_strict_profile() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::TempDir::new().expect("create temp dir for profile");

    let allowed_binary = dir.path().join("strict_allowed_true");
    std::fs::copy("/usr/bin/true", &allowed_binary).expect("copy /usr/bin/true into tempdir");
    let mut perms = std::fs::metadata(&allowed_binary)
        .expect("stat copied binary")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&allowed_binary, perms).expect("chmod copied binary");

    // Seatbelt path-literal matching is on the canonical vnode path.
    // macOS tempdirs live under `/var/folders/...`, itself a symlink to
    // `/private/var/folders/...`; without canonicalizing here the SBPL
    // literal would never match what the kernel actually resolves at
    // exec time (`from_entitlements` does this same canonicalization
    // for real profiles -- see e.g. policy.rs lines ~240 and ~262-280).
    let allowed_binary_canonical =
        std::fs::canonicalize(&allowed_binary).expect("canonicalize copied binary");

    let policy = SandboxPolicy {
        spawn_mode: SpawnMode::Strict,
        spawn_allowed_paths: vec![allowed_binary_canonical.clone(), PathBuf::from("/bin/bash")],
        ..SandboxPolicy::default()
    };
    let sbpl = build_sbpl_profile(&policy);

    let profile_path = dir.path().join("strict.sb");
    let mut f = std::fs::File::create(&profile_path).expect("create profile file");
    f.write_all(sbpl.as_bytes()).expect("write profile file");
    (dir, profile_path, allowed_binary_canonical)
}

#[test]
fn strict_mode_denies_system_path_but_allows_shell_and_allowlisted_binary() {
    if skip_unless_opted_in(
        "strict_mode_denies_system_path_but_allows_shell_and_allowlisted_binary",
    ) {
        return;
    }
    let (_dir, profile_path, allowed_binary) = write_strict_profile();
    let target = _dir.path().join("strict_mode_probe");

    // /bin/mkdir is a system-path coreutil, NOT in spawn_allowed_paths.
    // Under Allowlist mode this is exempted (see
    // `system_path_coreutil_runs_under_enforced_profile` above); under
    // Strict mode the MACOS_SYSTEM_EXEC_PATHS re-allow is dropped, so the
    // kernel must deny it.
    let mkdir_status = run_under_profile(&profile_path, &["/bin/mkdir", target.to_str().unwrap()]);
    assert!(
        !mkdir_status.success(),
        "/bin/mkdir must be kernel-denied under Strict mode (system-path \
         exemption is fenced), got exit status {mkdir_status:?}"
    );
    assert!(
        !target.exists(),
        "mkdir must not have actually run under the Strict-mode deny"
    );

    // A tempdir-local binary explicitly present in spawn_allowed_paths
    // must still exec successfully.
    let allowed_status = run_under_profile(&profile_path, &[allowed_binary.to_str().unwrap()]);
    assert!(
        allowed_status.success(),
        "a binary explicitly in spawn_allowed_paths must exec successfully \
         under Strict mode, got exit status {allowed_status:?}"
    );

    // bash itself (the shell the bash tool spawns) must still be
    // exec-able — the whole point of the Strict contract is "the bash
    // tool can still launch its shell; nothing else is implied".
    let bash_status = run_under_profile(&profile_path, &["/bin/bash", "-c", "true"]);
    assert!(
        bash_status.success(),
        "/bin/bash must remain exec-able under Strict mode so the bash \
         tool can still function, got exit status {bash_status:?}"
    );
}
