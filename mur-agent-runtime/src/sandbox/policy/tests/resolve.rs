use super::*;

#[test]
#[cfg(not(target_os = "windows"))]
fn resolve_binary_path_finds_executable_in_fs_exec_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_path = make_fake_executable(tmp.path(), "fake-tool");

    let resolved = resolve_binary_path("fake-tool", &[tmp.path().to_path_buf()]);
    assert_eq!(resolved, Some(bin_path));
}

#[test]
fn resolve_binary_path_drops_missing_binary_without_panic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let resolved = resolve_binary_path("definitely-does-not-exist", &[tmp.path().to_path_buf()]);
    assert_eq!(resolved, None);
}

/// The pairing that makes the drop *knowable*. Dropping a dead grant is
/// already covered by the test below; this pins that the drop is also
/// recorded, because a WARN in a log is not an answer to "what can this
/// agent write".
#[test]
fn a_dropped_dead_grant_is_recorded_with_its_verb_and_reason() {
    let home = tempfile::tempdir().unwrap();
    let gone = home.path().join("not-here");
    let mut ent = minimal_entitlements();
    ent.filesystem.read = vec![gone.display().to_string()];
    ent.filesystem.write = vec![gone.display().to_string()];
    let p = SandboxPolicy::from_entitlements(&ent, home.path());

    // Only the grant under test: the fixture's own paths are
    // environment-dependent, so an assertion over the whole list would be
    // about the CI runner rather than about this code.
    let mine: Vec<_> = p
        .dropped
        .iter()
        .filter(|d| d.path == gone.display().to_string())
        .collect();
    let verbs: Vec<&str> = mine.iter().map(|d| d.verb.as_str()).collect();
    assert!(verbs.contains(&"read"), "{:?}", p.dropped);
    assert!(verbs.contains(&"write"), "{:?}", p.dropped);
    assert!(
        mine.iter().all(|d| d.reason.contains("does not exist")),
        "{:?}",
        p.dropped
    );
}

/// The control: a grant whose path is present is never recorded as dropped.
///
/// Asserts about *this* grant rather than an empty list, because the
/// fixture's own entitlements are environment-dependent — `~/Documents`
/// exists on the macOS runner and not on a bare Ubuntu one, so a global
/// emptiness assertion passes locally and fails in CI for a reason that has
/// nothing to do with what this test is about.
#[test]
fn a_live_grant_records_no_drop() {
    let home = tempfile::tempdir().unwrap();
    let live = home.path().join("tree");
    std::fs::create_dir(&live).unwrap();
    let mut ent = minimal_entitlements();
    ent.filesystem.write = vec![live.display().to_string()];
    let p = SandboxPolicy::from_entitlements(&ent, home.path());
    assert!(
        !p.dropped
            .iter()
            .any(|d| d.path == live.display().to_string()),
        "a grant whose path exists must never be recorded as dropped: {:?}",
        p.dropped
    );
}

#[test]
fn from_entitlements_drops_dead_read_write_but_keeps_dead_deny() {
    // Issue 16 regression: a user-declared fs_read/fs_write entitlement
    // path that does not exist on disk (e.g. a removed git worktree)
    // must be dropped at profile-build time rather than emitted as a
    // dead SBPL `subpath` grant — a dead grant there was observed to
    // destabilize other, unrelated file-write* checks under the same
    // compiled sandbox policy (30s tool-call hangs, not EPERM). A dead
    // `fs_deny` entry, by contrast, must be KEPT verbatim: dropping it
    // would be fail-OPEN if the path later reappears.
    let tmp = tempfile::tempdir().expect("tempdir");
    let live_dir = tmp.path().join("live");
    std::fs::create_dir(&live_dir).expect("mkdir live");
    let dead_dir = tmp.path().join("dead");
    std::fs::create_dir(&dead_dir).expect("mkdir dead");
    std::fs::remove_dir(&dead_dir).expect("rmdir dead (now nonexistent)");

    let mut ent = minimal_entitlements();
    ent.filesystem.read = vec![
        live_dir.to_string_lossy().to_string(),
        dead_dir.to_string_lossy().to_string(),
    ];
    ent.filesystem.write = vec![
        live_dir.to_string_lossy().to_string(),
        dead_dir.to_string_lossy().to_string(),
    ];
    // Same dead path, but declared as a deny entry: must survive.
    ent.filesystem.deny = vec![dead_dir.to_string_lossy().to_string()];

    // Nest agent_home two levels inside the tempdir so the derived
    // channels dir (`agent_home.parent().parent()/channels`) stays
    // inside the tempdir too, rather than touching the real system /tmp.
    let agent_home = tmp.path().join("agents").join("dead-grant-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert!(
        policy.fs_read.contains(&live_dir),
        "live read path must be kept: {:?}",
        policy.fs_read
    );
    assert!(
        !policy.fs_read.contains(&dead_dir),
        "dead read path must be dropped: {:?}",
        policy.fs_read
    );
    assert!(
        policy.fs_write.contains(&live_dir),
        "live write path must be kept: {:?}",
        policy.fs_write
    );
    assert!(
        !policy.fs_write.contains(&dead_dir),
        "dead write path must be dropped: {:?}",
        policy.fs_write
    );
    assert!(
        policy.fs_deny.contains(&dead_dir),
        "dead deny path must be KEPT verbatim (dropping would be fail-open): {:?}",
        policy.fs_deny
    );

    #[cfg(target_os = "macos")]
    {
        let sbpl = crate::sandbox::macos::build_sbpl_profile(&policy);
        let live_p = live_dir.to_string_lossy();
        let dead_p = dead_dir.to_string_lossy();
        assert!(
            sbpl.contains(&format!("(allow file-write* (subpath \"{live_p}\"))")),
            "SBPL must contain an allow-write subpath for the live path"
        );
        assert!(
            !sbpl.contains(&format!("(allow file-write* (subpath \"{dead_p}\"))")),
            "SBPL must NOT contain an allow-write subpath for the dropped dead path"
        );
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn from_entitlements_adds_resolved_path_for_symlinked_fs_grants() {
    // Seatbelt matches the RESOLVED path. A grant named by its link path
    // (a relocated `~/Library/Caches/ms-playwright`) silently denied every
    // write under it until the user re-granted the real path (F1, run3a).
    // Both forms must reach the kernel: the link path for Landlock and the
    // file tools, the resolved one for Seatbelt. A deny gets the same
    // treatment, so a symlink cannot be used to walk around it.
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).expect("mkdir real");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let real_canon = std::fs::canonicalize(&real).expect("canonicalize real");

    let mut ent = minimal_entitlements();
    ent.filesystem.read = vec![link.to_string_lossy().to_string()];
    ent.filesystem.write = vec![link.to_string_lossy().to_string()];
    ent.filesystem.deny = vec![link.to_string_lossy().to_string()];
    let agent_home = tmp.path().join("agents").join("symlink-grant-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    for (verb, list) in [
        ("read", &policy.fs_read),
        ("write", &policy.fs_write),
        ("deny", &policy.fs_deny),
    ] {
        assert!(list.contains(&link), "{verb} keeps the link path: {list:?}");
        assert!(
            list.contains(&real_canon),
            "{verb} adds the resolved path: {list:?}"
        );
        assert_eq!(
            list.iter().filter(|p| **p == real_canon).count(),
            1,
            "{verb} adds the resolved path once: {list:?}"
        );
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn from_entitlements_resolves_spawn_allowed_and_drops_unresolved() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_path = make_fake_executable(tmp.path(), "fake-tool");

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec![
        bin_path.to_string_lossy().to_string(),
        "definitely-does-not-exist".to_string(),
    ];

    let agent_home = PathBuf::from("/tmp/agent_home_spawn");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert_eq!(policy.spawn_mode, SpawnMode::Allowlist);
    let expected_canonical_bin = std::fs::canonicalize(&bin_path).expect("canonicalize fake tool");
    // Tempdirs may sit behind a symlinked ancestor (e.g. /var ->
    // /private/var on macOS), in which case BOTH the original and
    // canonical forms are kept. Assert the canonical form is present and
    // that no unrelated binary name leaked in.
    assert!(
        policy.spawn_allowed_paths.contains(&expected_canonical_bin),
        "spawn_allowed_paths must contain the canonical fake tool path: {:?}",
        policy.spawn_allowed_paths
    );
    assert!(
        policy
            .spawn_allowed_paths
            .iter()
            .all(|p| p.ends_with("fake-tool")),
        "no unrelated binary name should have leaked into spawn_allowed_paths: {:?}",
        policy.spawn_allowed_paths
    );
}

/// The build lane: an explicit directory grant becomes an exec prefix, so
/// a toolchain can run binaries it compiles itself (cargo build scripts,
/// proc-macro shims, test executables) at paths that do not exist when
/// the entitlement is written. Fails closed on anything unusable, and
/// refuses a grant so broad it is not a lane at all.
#[test]
fn build_lane_grants_a_directory_prefix_and_fails_closed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).expect("mkdir target");
    let a_file = tmp.path().join("not-a-dir");
    std::fs::write(&a_file, b"x").expect("write file");
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed_dirs = vec![
        target.to_string_lossy().to_string(),
        a_file.to_string_lossy().to_string(), // not a directory
        "/definitely/does/not/exist".to_string(), // missing
        "/".to_string(),                      // guarded: not a lane
        home.to_string_lossy().to_string(),   // guarded: whole home
    ];

    let policy =
        SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/agent_home_build_lane"));

    let canon_target = std::fs::canonicalize(&target).expect("canonicalize target");
    assert!(
        policy.spawn_allowed_prefixes.contains(&canon_target),
        "the build-output directory must become an exec prefix: {:?}",
        policy.spawn_allowed_prefixes
    );
    for bad in [
        PathBuf::from("/"),
        home.clone(),
        PathBuf::from("/definitely/does/not/exist"),
    ] {
        assert!(
            !policy.spawn_allowed_prefixes.contains(&bad),
            "must not grant {bad:?}: {:?}",
            policy.spawn_allowed_prefixes
        );
    }
    assert!(
        !policy
            .spawn_allowed_prefixes
            .iter()
            .any(|p| p.ends_with("not-a-dir")),
        "a plain file is not a lane: {:?}",
        policy.spawn_allowed_prefixes
    );
}

#[test]
fn build_lane_grants_the_matching_path_in_linked_worktrees() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("main");
    let worktree = tmp.path().join("worktree");

    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    git(&["init", main.to_str().expect("UTF-8 main path")]);
    git(&[
        "-C",
        main.to_str().expect("UTF-8 main path"),
        "config",
        "user.email",
        "test@example.com",
    ]);
    git(&[
        "-C",
        main.to_str().expect("UTF-8 main path"),
        "config",
        "user.name",
        "MUR Test",
    ]);
    std::fs::write(main.join("README.md"), "fixture").expect("write fixture");
    git(&[
        "-C",
        main.to_str().expect("UTF-8 main path"),
        "add",
        "README.md",
    ]);
    git(&[
        "-C",
        main.to_str().expect("UTF-8 main path"),
        "commit",
        "-m",
        "fixture",
    ]);
    git(&[
        "-C",
        main.to_str().expect("UTF-8 main path"),
        "worktree",
        "add",
        "-b",
        "linked",
        worktree.to_str().expect("UTF-8 worktree path"),
    ]);

    let main_target = main.join("target");
    let worktree_target = worktree.join("target");
    std::fs::create_dir_all(&main_target).expect("mkdir main target");
    std::fs::create_dir_all(&worktree_target).expect("mkdir worktree target");

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed_dirs = vec![main_target.to_string_lossy().to_string()];

    let policy = SandboxPolicy::from_entitlements(&ent, &tmp.path().join("agent-home"));
    let expected = std::fs::canonicalize(&worktree_target).expect("canonicalize target");
    assert!(
        policy.spawn_allowed_prefixes.contains(&expected),
        "linked-worktree target must become an exec prefix: {:?}",
        policy.spawn_allowed_prefixes
    );

    let build_script = expected
        .join("debug")
        .join("build")
        .join("crate-hash")
        .join("build-script-build");
    assert!(
        policy
            .spawn_allowed_prefixes
            .iter()
            .any(|prefix| build_script.starts_with(prefix)),
        "Cargo build script must fall under an allowed prefix: {:?}",
        policy.spawn_allowed_prefixes
    );
}

#[test]
#[cfg(unix)]
fn from_entitlements_resolves_symlinked_spawn_entry_to_canonical_prefix() {
    // Issue 17: an allowlist entry may be an absolute path to a symlink
    // (e.g. a shim/wrapper) rather than the real binary. The resolved
    // literal must be the CANONICAL target, and the derived prefix must
    // be computed from that canonical path — a `<pkg>/<version>/bin/tool`
    // layout should yield `<pkg>/<version>` as the prefix (grandparent,
    // since the parent is named `bin`), not the shim's own directory.
    let tmp = tempfile::tempdir().expect("tempdir");

    let real_bin_dir = tmp.path().join("pkg").join("1.0").join("bin");
    std::fs::create_dir_all(&real_bin_dir).expect("mkdir real bin dir");
    let real_tool = make_fake_executable(&real_bin_dir, "tool");

    let shim_dir = tmp.path().join("shim-bin");
    std::fs::create_dir_all(&shim_dir).expect("mkdir shim dir");
    let shim_tool = shim_dir.join("tool");
    std::os::unix::fs::symlink(&real_tool, &shim_tool).expect("symlink shim -> real tool");

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec![shim_tool.to_string_lossy().to_string()];

    let agent_home = tmp.path().join("agents").join("symlink-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    let expected_canonical = std::fs::canonicalize(&real_tool).expect("canonicalize real tool");
    assert!(
        policy.spawn_allowed_paths.contains(&expected_canonical),
        "spawn_allowed_paths must contain the canonical target of the symlink: {:?}",
        policy.spawn_allowed_paths
    );

    let expected_prefix =
        std::fs::canonicalize(tmp.path().join("pkg").join("1.0")).expect("canonicalize pkg/1.0");
    assert!(
        policy.spawn_allowed_prefixes.contains(&expected_prefix),
        "spawn_allowed_prefixes must contain the grandparent pkg/1.0 dir \
             (parent is `bin`): {:?}",
        policy.spawn_allowed_prefixes
    );
}

#[test]
#[cfg(unix)]
fn from_entitlements_spawn_prefix_is_immediate_parent_when_not_under_bin() {
    // When the binary's parent directory is NOT named `bin`, the prefix
    // must be that immediate parent itself (no grandparent hop, no
    // broad-root guard triggered) — deterministic, without needing to
    // fake a real filesystem root or /usr.
    let tmp = tempfile::tempdir().expect("tempdir");
    let just_a_dir = tmp.path().join("just-a-dir");
    std::fs::create_dir_all(&just_a_dir).expect("mkdir just-a-dir");
    let tool_path = make_fake_executable(&just_a_dir, "tool");

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec![tool_path.to_string_lossy().to_string()];

    let agent_home = tmp.path().join("agents").join("non-bin-prefix-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    let expected_prefix = std::fs::canonicalize(&just_a_dir).expect("canonicalize just-a-dir");
    // Tempdirs may sit behind a symlinked ancestor (e.g. /var ->
    // /private/var on macOS), in which case BOTH the original and
    // canonical forms are kept. Assert the canonical form is present and
    // that no unrelated directory name leaked in.
    assert!(
        policy.spawn_allowed_prefixes.contains(&expected_prefix),
        "prefixes must contain the canonical just-a-dir path: {:?}",
        policy.spawn_allowed_prefixes
    );
    assert!(
        policy
            .spawn_allowed_prefixes
            .iter()
            .all(|p| p.ends_with("just-a-dir")),
        "no unrelated directory should have leaked into spawn_allowed_prefixes: {:?}",
        policy.spawn_allowed_prefixes
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn from_entitlements_empty_spawn_allowlist_yields_empty_paths_and_prefixes() {
    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec![];

    let agent_home = PathBuf::from("/tmp/agent_home_empty_spawn");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert_eq!(policy.spawn_mode, SpawnMode::Allowlist);
    assert!(
        policy.spawn_allowed_paths.is_empty(),
        "empty allowlist must yield no resolved paths: {:?}",
        policy.spawn_allowed_paths
    );
    assert!(
        policy.spawn_allowed_prefixes.is_empty(),
        "empty allowlist must yield no derived prefixes: {:?}",
        policy.spawn_allowed_prefixes
    );
}

#[test]
#[cfg(unix)]
fn strict_mode_seeds_shell_into_spawn_allowed() {
    // Decision (i): in Strict mode the runtime itself guarantees the
    // bash TOOL stays functional by resolving the same `bash` binary
    // `tools/bash.rs` spawns (a PATH lookup) and auto-seeding its
    // canonical path into `spawn_allowed_paths` -- even when the
    // profile author declared an empty allowlist.
    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Strict;
    ent.processes.spawn.allowed = vec![];

    let agent_home = PathBuf::from("/tmp/agent_home_strict_shell_seed");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    assert_eq!(policy.spawn_mode, SpawnMode::Strict);
    assert!(
        policy
            .spawn_allowed_paths
            .iter()
            .any(|p| p.ends_with("bash")),
        "strict mode must auto-seed a resolved bash path even with an \
             empty allowlist: {:?}",
        policy.spawn_allowed_paths
    );
    // On a normal macOS/unix host `bash` resolves via PATH to the
    // canonical system shell at /bin/bash.
    let canonical_bash = std::fs::canonicalize("/bin/bash");
    if let Ok(canonical_bash) = canonical_bash {
        assert!(
            policy.spawn_allowed_paths.contains(&canonical_bash),
            "expected the canonical /bin/bash to be seeded into \
                 spawn_allowed_paths: {:?}",
            policy.spawn_allowed_paths
        );
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn fake_rustup_toolchain_bin_is_searched() {
    // Issue 17: `~/.cargo/bin/cargo` is a rustup PROXY that re-execs the
    // real binary under `<rustup_home>/toolchains/<toolchain>/bin/` at
    // runtime — that real exec path must be discoverable via a bare
    // `cargo` allowlist entry, without a real rustup install present.
    let tmp = tempfile::tempdir().expect("tempdir");
    let toolchain_bin = tmp.path().join("toolchains").join("tc1").join("bin");
    std::fs::create_dir_all(&toolchain_bin).expect("mkdir toolchain bin dir");
    let cargo_path = make_fake_executable(&toolchain_bin, "cargo");

    let rustup_home = tmp.path().to_path_buf();
    let _env = mur_common::test_env::EnvGuard::set([("RUSTUP_HOME", &rustup_home)]);

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec!["cargo".to_string()];

    let agent_home = tmp.path().join("agents").join("rustup-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    let expected_canonical =
        std::fs::canonicalize(&cargo_path).expect("canonicalize toolchain cargo");
    assert!(
        policy.spawn_allowed_paths.contains(&expected_canonical),
        "spawn_allowed_paths must contain the rustup toolchain's cargo: {:?}",
        policy.spawn_allowed_paths
    );
}

#[test]
#[cfg(unix)]
fn both_path_forms_kept_for_symlinked_ancestor() {
    // Issue 17: when an ANCESTOR directory of an allowlisted absolute
    // path is a symlink (not the file itself), Seatbelt's exec-path
    // check may observe either the original (symlink-form) path or the
    // canonicalized one depending on how the process is launched — both
    // forms must be granted.
    let tmp = tempfile::tempdir().expect("tempdir");

    let real_bin_dir = tmp.path().join("pkg").join("bin");
    std::fs::create_dir_all(&real_bin_dir).expect("mkdir real bin dir");
    let real_tool = make_fake_executable(&real_bin_dir, "tool");

    let link_pkg = tmp.path().join("link-pkg");
    std::os::unix::fs::symlink(tmp.path().join("pkg"), &link_pkg).expect("symlink link-pkg -> pkg");

    let symlink_form_tool = tmp.path().join("link-pkg").join("bin").join("tool");

    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed = vec![symlink_form_tool.to_string_lossy().to_string()];

    let agent_home = tmp.path().join("agents").join("symlink-ancestor-test");
    let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

    let expected_canonical = std::fs::canonicalize(&real_tool).expect("canonicalize real tool");
    // The symlink-form entry stored in `spawn_allowed_paths` is the
    // ORIGINAL entitlement literal, reconstructed verbatim from the
    // string via `Path::new` — never itself canonicalized (only the
    // resolved `canon` value is). `symlink_form_tool` was built the
    // same way (joined on `tmp.path()` as returned, before any
    // canonicalization), so it is the exact expected value — no need
    // to canonicalize `tmp.path()` here, since doing so would collapse
    // the `link-pkg` symlink hop this test specifically exercises.
    assert!(
        policy.spawn_allowed_paths.contains(&symlink_form_tool),
        "spawn_allowed_paths must contain the symlink-form path: {:?}",
        policy.spawn_allowed_paths
    );
    assert!(
        policy.spawn_allowed_paths.contains(&expected_canonical),
        "spawn_allowed_paths must contain the canonical form: {:?}",
        policy.spawn_allowed_paths
    );
}

/// A `<home>/<dir>/bin/<tool>` binary must not widen to `<home>/<dir>`:
/// `~/.local/bin/mur` granting all of `~/.local` made every interpreter
/// under `~/.local/share` (uv-managed Pythons, pipx venvs) executable.
#[test]
fn spawn_prefix_does_not_widen_to_a_direct_child_of_home() {
    let home = PathBuf::from("/Users/someone");
    for tool in [".local/bin/mur", ".cargo/bin/cargo"] {
        let literal = home.join(tool);
        let bin_dir = literal.parent().unwrap().to_path_buf();
        assert_eq!(
            compute_spawn_prefix(&literal, &home),
            bin_dir,
            "{tool} must be confined to its own bin/ dir"
        );
    }
}

/// Toolchains one level deeper under home still get their package prefix,
/// so sibling `lib`/`libexec` dirs keep working.
#[test]
fn spawn_prefix_still_widens_below_a_home_child() {
    let home = PathBuf::from("/Users/someone");
    let literal = home.join(".mur/tools/serena/2.0.0/bin/serena");
    assert_eq!(
        compute_spawn_prefix(&literal, &home),
        home.join(".mur/tools/serena/2.0.0")
    );
}
