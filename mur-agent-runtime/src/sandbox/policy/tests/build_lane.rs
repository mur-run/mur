//! Build-lane (`processes.spawn.allowed_dirs`) grants for directories that do
//! not exist at seal time (#2058): the canonical parent plus the literal leaf.

use super::*;

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).expect("canonicalize")
}

fn seal_with(dirs: Vec<String>, agent_home: &Path) -> SandboxPolicy {
    let mut ent = minimal_entitlements();
    ent.processes.spawn.mode = SpawnMode::Allowlist;
    ent.processes.spawn.allowed_dirs = dirs;
    SandboxPolicy::from_entitlements(&ent, agent_home)
}

/// The fleet order: members start, then a worktree's `target/` is created. The
/// grant must already cover it, with no restart.
#[test]
fn a_directory_created_after_seal_is_already_granted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let later = tmp.path().join("job-worktree-target");

    let policy = seal_with(
        vec![later.to_string_lossy().to_string()],
        &tmp.path().join("agent-home"),
    );
    std::fs::create_dir_all(&later).expect("created after the seal");

    let want = canon(tmp.path()).join("job-worktree-target");
    assert!(
        policy.spawn_allowed_prefixes.contains(&want),
        "the pending directory must be granted as canonical parent + leaf: {:?}",
        policy.spawn_allowed_prefixes
    );
    assert_eq!(
        want,
        canon(&later),
        "the grant is where the directory landed"
    );
    assert!(
        !policy.dropped.iter().any(|d| d.verb == "spawn"),
        "a grant that is installed must not be reported as dropped: {:?}",
        policy.dropped
    );
}

#[test]
fn a_missing_parent_still_drops_the_grant_as_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let deep = tmp.path().join("no-parent").join("target");

    let policy = seal_with(
        vec![deep.to_string_lossy().to_string()],
        &tmp.path().join("agent-home"),
    );
    assert!(policy.spawn_allowed_prefixes.is_empty());
    let d = policy
        .dropped
        .iter()
        .find(|d| d.verb == "spawn")
        .expect("recorded");
    assert!(d.reason.contains("neither did its parent"), "{}", d.reason);
}

/// A trusted parent must not imply a safe child: the guard runs on the JOINED
/// path. `$HOME`, `/`, `/usr`, `/opt` and a bare `/Volumes/<mount>` are refused
/// whether they exist (live form) or only their parent does (pending form).
#[test]
fn guarded_paths_are_refused_through_the_parent_plus_leaf_form() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // `$HOME` reached as a pending leaf: parent exists, home itself does not.
    // The resolver canonicalizes the parent (macOS `/var` -> `/private/var`,
    // Windows `\\?\` prefix), so the home it is compared with must be built from
    // the canonical parent too.
    let fake_home = canon(tmp.path()).join("fake-home");
    assert!(!fake_home.exists());
    assert_eq!(
        resolve_build_lane_dir(&fake_home, &fake_home.clone()).err(),
        Some(REASON_TOO_BROAD),
        "a pending path equal to home is not a build lane"
    );
    guarded_unix_roots_are_refused(tmp.path());
}

/// The Unix-rooted half: `/usr`, `/opt` and `/Volumes/<mount>` mean nothing on
/// Windows, where they are neither absolute nor guarded.
#[cfg(unix)]
fn guarded_unix_roots_are_refused(scratch: &Path) {
    // The pending leaf directly under `/`: `/` + `usr`-style names.
    assert!(
        resolve_build_lane_dir(Path::new("/definitely-not-a-real-top-level"), &home_dir())
            .is_ok_and(|d| matches!(d, BuildLaneDir::Pending(_))),
        "an ordinary pending leaf is fine; only guarded joins are refused"
    );

    let home = home_dir();
    for guarded in [
        "/",
        "/usr",
        "/opt",
        "/opt/homebrew",
        "/Volumes/no-such-mount",
    ] {
        assert!(
            resolve_build_lane_dir(Path::new(guarded), &home).is_err(),
            "{guarded} must be refused whether it exists or not"
        );
    }
    assert!(resolve_build_lane_dir(&home, &home).is_err(), "$HOME");

    // End to end through the seal, not just the helper.
    let policy = seal_with(
        vec![
            "/".into(),
            "/usr".into(),
            "/opt".into(),
            "/Volumes/no-such-mount".into(),
        ],
        &scratch.join("agent-home"),
    );
    for bad in ["/", "/usr", "/opt", "/Volumes/no-such-mount"] {
        assert!(
            !policy.spawn_allowed_prefixes.contains(&PathBuf::from(bad)),
            "{bad} leaked into the grants: {:?}",
            policy.spawn_allowed_prefixes
        );
    }
}

#[cfg(not(unix))]
fn guarded_unix_roots_are_refused(_scratch: &Path) {}

#[test]
fn a_leaf_cannot_escape_the_canonical_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).expect("mkdir");
    let home = home_dir();

    for sneaky in [
        real.join("..").join("escape"),
        tmp.path().join("gone").join("..").join("escape"),
        real.join(".."),
        real.join("."),
    ] {
        let got = resolve_build_lane_dir(&sneaky, &home);
        if let Ok(BuildLaneDir::Pending(p)) = &got {
            panic!("{sneaky:?} must not become a pending grant, got {p:?}");
        }
        if let Ok(BuildLaneDir::Live(p)) = &got {
            // `real/..` and `real/.` exist, so they resolve normally.
            assert_eq!(*p, canon(&sneaky), "a live path is granted canonically");
        }
    }
    assert_eq!(
        resolve_build_lane_dir(&real.join("..").join("escape"), &home).err(),
        Some(REASON_UNSAFE_LEAF)
    );
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_widen_the_pending_grant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).expect("mkdir");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let home = home_dir();

    // A symlinked PARENT is resolved: the grant lands on the real directory.
    match resolve_build_lane_dir(&link.join("target"), &home) {
        Ok(BuildLaneDir::Pending(p)) => assert_eq!(p, canon(&real).join("target")),
        other => panic!(
            "expected a pending grant on the real parent, got {:?}",
            other.err()
        ),
    }

    // A dangling symlink LEAF is refused: where it points once it resolves is
    // not something the operator named.
    let dangling = tmp.path().join("dangling");
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), &dangling).expect("symlink");
    assert_eq!(
        resolve_build_lane_dir(&dangling, &home).err(),
        Some(REASON_UNSAFE_LEAF)
    );
}

/// The fixes differ, so the reasons must: create-and-restart, narrow the
/// directory, or name a plain leaf.
#[test]
fn each_refusal_has_its_own_reason() {
    let reasons = [
        REASON_MISSING,
        REASON_TOO_BROAD,
        REASON_NOT_A_DIR,
        REASON_UNSAFE_LEAF,
    ];
    for (i, a) in reasons.iter().enumerate() {
        for b in &reasons[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// macOS is the only kernel that reads `spawn_allowed_prefixes` (Landlock has
/// no exec-prefix rule, so on Linux the build lane adds no kernel grant and
/// nothing can widen). An SBPL `subpath` matches by path at check time, so a
/// rule written for a directory that does not exist yet applies once it does.
#[cfg(target_os = "macos")]
#[test]
fn a_pending_grant_renders_as_an_exec_subpath_in_the_sbpl_profile() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let later = tmp.path().join("job-worktree-target");
    let policy = seal_with(
        vec![later.to_string_lossy().to_string()],
        &tmp.path().join("agent-home"),
    );

    let sbpl = crate::sandbox::macos::build_sbpl_profile(&policy);
    let want = canon(tmp.path()).join("job-worktree-target");
    let rule = format!("(allow process-exec* (subpath \"{}\"))", want.display());
    assert!(sbpl.contains(&rule), "missing {rule}\n{sbpl}");
    // Only the leaf is granted, never its parent.
    let parent_rule = format!(
        "(allow process-exec* (subpath \"{}\"))",
        canon(tmp.path()).display()
    );
    assert!(
        !sbpl.contains(&parent_rule),
        "the parent must not be granted"
    );
}

/// A real repo with one linked worktree, or `None` when git is unavailable.
fn repo_with_worktree(tmp: &Path) -> Option<(PathBuf, PathBuf)> {
    let main = tmp.join("main");
    std::fs::create_dir_all(&main).ok()?;
    let git = |args: &[&str]| -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&main)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q"]) {
        return None;
    }
    let _ = git(&["config", "user.email", "t@example.com"]);
    let _ = git(&["config", "user.name", "t"]);
    std::fs::write(main.join("f.txt"), "x").ok()?;
    let _ = git(&["add", "f.txt"]);
    let _ = git(&["commit", "-qm", "init"]);
    let wt = tmp.join("wt");
    if !git(&["worktree", "add", "-q", wt.to_str()?, "-b", "feat"]) {
        return None;
    }
    Some((main, wt))
}

/// #2058 acceptance: a fleet job in a worktree whose `target/` the first
/// build has not created yet. The worktree is registered before the seal; its
/// `target/` appears after. The grant must already cover it.
#[test]
fn a_missing_target_under_a_linked_worktree_is_granted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some((main, wt)) = repo_with_worktree(tmp.path()) else {
        eprintln!("skipping: git unavailable or worktree creation failed");
        return;
    };
    let target = main.join("target");
    assert!(!target.exists() && !wt.join("target").exists());

    let policy = seal_with(
        vec![target.to_string_lossy().to_string()],
        &tmp.path().join("agent-home"),
    );
    std::fs::create_dir_all(wt.join("target")).expect("created after the seal");

    let want = canon(&wt).join("target");
    assert!(
        policy.spawn_allowed_prefixes.contains(&want),
        "the worktree's pending target/ must be granted: {:?}",
        policy.spawn_allowed_prefixes
    );
    assert_eq!(want, canon(&wt.join("target")));
    assert!(
        policy
            .spawn_allowed_prefixes
            .contains(&canon(&main).join("target")),
        "the configured directory itself is still granted: {:?}",
        policy.spawn_allowed_prefixes
    );
    assert!(
        !policy.dropped.iter().any(|d| d.verb == "spawn"),
        "{:?}",
        policy.dropped
    );
}

/// A worktree deleted from disk contributes no grant, pending or not.
#[test]
fn a_pruned_worktree_gets_no_pending_grant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some((main, wt)) = repo_with_worktree(tmp.path()) else {
        eprintln!("skipping: git unavailable or worktree creation failed");
        return;
    };
    let wt_canon = canon(&wt);
    std::fs::remove_dir_all(&wt).expect("remove worktree");

    let policy = seal_with(
        vec![main.join("target").to_string_lossy().to_string()],
        &tmp.path().join("agent-home"),
    );
    assert!(
        !policy
            .spawn_allowed_prefixes
            .iter()
            .any(|p| p.starts_with(&wt_canon)),
        "{:?}",
        policy.spawn_allowed_prefixes
    );
}
