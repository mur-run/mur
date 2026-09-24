use super::*;

/// R5: the sandbox force-grants `agent_home` ("runtime cannot function
/// without it" — `sandbox::policy::from_entitlements`), but the file tools
/// were handed the RAW profile entitlement, so `write_file` refused a path
/// the kernel would have allowed and the agent could not write inside its
/// own home (`path not write-entitled: ~/.mur/agents/<name>/…`, observed
/// 2026-09-13; it then reached for `/tmp` and tripped the withdrawal).
///
/// Only `agent_home` is aligned. The other paths the sandbox force-grants
/// (`channels/`, `index/channels/`, `open-items.jsonl`) stay out on
/// purpose: those are written by the RUNTIME, and this gate bounds what
/// the MODEL may write. Granting them here would let a prompt-injected
/// agent forge channel events through `write_file`.
/// Same grant, other layer: `write_file`/`edit_file` must accept the path
/// the system prompt sends the agent to. The sandbox granting it is not
/// enough — the kernel allowed the write while this gate refused it, which
/// is exactly how the agent ended up probing `/tmp`.
#[test]
fn the_agents_own_artifacts_dir_is_writable_by_the_file_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = std::fs::canonicalize(tmp.path()).unwrap();
    let agent_home = mur_home.join("agents/rustsmith");
    std::fs::create_dir_all(&agent_home).unwrap();
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let fs = for_file_tools(FilesystemEntitlement::default(), &agent_home);

    check_write_entitlement(
        &fs,
        &mur_home.join("artifacts/rustsmith/task4/check.rs"),
        &chain,
    )
    .expect("the path the system prompt names must be writable");

    check_write_entitlement(&fs, &mur_home.join("artifacts/pm/report.md"), &chain)
        .expect_err("a sibling agent's artifacts must not be writable");
}

#[test]
fn agent_home_is_writable_by_the_file_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let agent_home = home.join("agents/rustsmith");
    std::fs::create_dir_all(&agent_home).unwrap();
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();

    // Grants nothing — exactly the profile rustsmith had for its own home.
    let fs = for_file_tools(FilesystemEntitlement::default(), &agent_home);

    check_write_entitlement(&fs, &agent_home.join("task4_check.rs"), &chain)
        .expect("an agent must be able to write inside its own home");

    // Negative controls: the carve-outs still hold, so the line above is a
    // scoped grant and not a blanket allow.
    check_write_entitlement(&fs, &agent_home.join("profile.yaml"), &chain)
        .expect_err("the agent's own profile stays denied (#712)");
    check_write_entitlement(&fs, &home.join("agents/pm/notes.md"), &chain)
        .expect_err("a sibling agent's home is not granted");
    check_write_entitlement(&fs, &home.join("channels/x/events.jsonl"), &chain)
        .expect_err("runtime-owned channel store must NOT be model-writable");
}

#[test]
fn launch_chain_beats_an_explicit_write_grant() {
    let tmp = tempfile::tempdir().unwrap();
    // Canonical base: the gate compares canonicalized paths, and on macOS
    // /var is a symlink to /private/var — raw tempdir paths would never
    // match the canonicalized grant roots the check computes.
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let agents = home.join("agents");
    let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
        &agents.join("mur"),
        &home.join("bin"),
        &home.join("home"),
    );

    // The most permissive grant a user could write.
    let fs = FilesystemEntitlement {
        write: vec![home.to_string_lossy().into_owned()],
        ..Default::default()
    };

    let err = check_write_entitlement(&fs, &agents.join("pm/profile.yaml"), &chain)
        .expect_err("a sibling profile must be refused even under a grant covering it");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("entitlements"),
        "error must explain why: {msg}"
    );

    // Negative control: the same grant still works for a path outside the
    // set, so the refusal above is the launch chain and not a broken check.
    check_write_entitlement(&fs, &home.join("skills/x.yaml"), &chain)
        .expect("unprotected path under the same grant must still be allowed");
}

/// The regression this helper exists for: a `~`-written grant must cover
/// the expanded path, because the sandbox builder already expands it. The
/// probe dir deliberately does NOT exist, so `canonicalize` fails on both
/// sides and the assertion turns purely on the expansion — no dependency
/// on the test host's home layout.
#[test]
fn tilde_grant_is_expanded_like_the_sandbox() {
    let home = dirs::home_dir().expect("home dir");
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let fs = FilesystemEntitlement {
        write: vec!["~/mur-tilde-grant-probe".to_string()],
        ..Default::default()
    };

    check_write_entitlement(&fs, &home.join("mur-tilde-grant-probe/photo.jpg"), &chain)
        .expect("a ~ grant must cover the path it expands to");

    // Negative control: expansion must not widen the grant to everything,
    // or the assertion above would pass on a helper that always says yes.
    check_write_entitlement(
        &fs,
        Path::new("/tmp/mur-tilde-grant-probe/photo.jpg"),
        &chain,
    )
    .expect_err("grant must stay scoped to the expanded root");
}

/// Same expansion, deny side. `detect_warnings` tells users to deny
/// `~/.ssh`; before this helper that entry was inert at the tool gate.
/// Every path here sits under a probe dir that does NOT exist, grant and
/// deny alike. That is not tidiness: on Windows `canonicalize` succeeds
/// only for a real path and returns a `\\?\` UNC prefix, so an earlier
/// version of this test that mixed a real root (UNC) with an absent one
/// (verbatim) compared two spellings of the same place — it passed on
/// Unix and failed on Windows CI. Keeping both sides absent makes the
/// assertion turn on the tilde expansion, which is what it is about.
#[test]
fn tilde_deny_is_expanded_too() {
    let home = dirs::home_dir().expect("home dir");
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let fs = FilesystemEntitlement {
        write: vec!["~/mur-deny-probe".to_string()],
        deny: vec!["~/mur-deny-probe/keys".to_string()],
        ..Default::default()
    };

    check_write_entitlement(&fs, &home.join("mur-deny-probe/keys/id_ed25519"), &chain)
        .expect_err("a ~ deny entry must outrank a ~ grant covering it");

    // Negative control: the surrounding grant still works, so the refusal
    // above is the deny entry and not a grant that never matched.
    check_write_entitlement(&fs, &home.join("mur-deny-probe/notes.md"), &chain)
        .expect("the write grant itself must still hold");
}

fn eperm() -> std::io::Error {
    std::io::Error::from_raw_os_error(1) // EPERM, "Operation not permitted"
}

#[test]
fn removable_eperm_matches_only_volumes_path() {
    assert!(is_removable_volume_eperm(
        Path::new("/Volumes/Ext/spec.md"),
        &eperm()
    ));
    // Same EPERM but NOT under /Volumes → not our case.
    assert!(!is_removable_volume_eperm(
        Path::new("/Users/me/spec.md"),
        &eperm()
    ));
}

#[test]
fn removable_eperm_ignores_eacces() {
    // Plain PermissionDenied (EACCES = os error 13) under /Volumes must
    // NOT be hijacked — only the exact EPERM (os error 1) qualifies.
    let eacces = std::io::Error::from_raw_os_error(13);
    assert!(!is_removable_volume_eperm(
        Path::new("/Volumes/Ext/spec.md"),
        &eacces
    ));
}

#[test]
fn format_io_error_appends_hint_on_volumes_eperm() {
    let msg = format_io_error(
        "read",
        Path::new("/Volumes/Ext/spec.md"),
        Path::new("/Volumes/Ext"),
        &eperm(),
    );
    assert!(msg.contains("relative to session cwd /Volumes/Ext"));
    assert!(msg.contains(REMOVABLE_VOLUME_EPERM_HINT));
}

#[test]
fn format_io_error_plain_error_has_no_hint() {
    let not_found = std::io::Error::from_raw_os_error(2); // ENOENT
    let msg = format_io_error(
        "read",
        Path::new("/Users/me/spec.md"),
        Path::new("/Users/me"),
        &not_found,
    );
    assert!(msg.contains("relative to session cwd /Users/me"));
    assert!(!msg.contains(REMOVABLE_VOLUME_EPERM_HINT));
}

#[test]
fn resolve_path_expands_tilde() {
    let home = dirs::home_dir().unwrap();
    let wd = Path::new("/tmp/wd");
    assert_eq!(resolve_path(wd, "~/.mur/skills"), home.join(".mur/skills"));
    assert_eq!(resolve_path(wd, "~"), home);
    assert_eq!(resolve_path(wd, "/abs/x"), PathBuf::from("/abs/x"));
    assert_eq!(resolve_path(wd, "rel/x"), wd.join("rel/x"));
    // `~user` form is not expanded — treated as a relative name.
    assert_eq!(resolve_path(wd, "~other/x"), wd.join("~other/x"));
}

#[test]
fn self_protected_denies_own_profile_despite_write_grant() {
    // Issue #712: a write grant covering the whole agent dir must not
    // let the file tools write the agent's own profile.yaml/identity.key.
    let tmp = tempfile::tempdir().expect("tempdir");
    let agent_home = tmp.path().join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(agent_home.join("profile.yaml"), "name: mur\n").unwrap();
    std::fs::write(agent_home.join("identity.key"), "KEY").unwrap();
    let fs = for_file_tools(
        FilesystemEntitlement {
            read: vec![],
            write: vec![agent_home.to_string_lossy().into_owned()],
            deny: vec![],
        },
        &agent_home,
    );
    let canonical_home = std::fs::canonicalize(&agent_home).unwrap();
    for f in ["profile.yaml", "identity.key"] {
        assert!(
            check_write_entitlement(
                &fs,
                &canonical_home.join(f),
                &crate::sandbox::launch_chain::LaunchChain::inert(),
            )
            .is_err(),
            "{f} must be write-denied despite the agent-dir grant"
        );
    }
    // The rest of the agent dir stays writable (running.lock etc.).
    assert!(
        check_write_entitlement(
            &fs,
            &canonical_home.join("running.lock"),
            &crate::sandbox::launch_chain::LaunchChain::inert(),
        )
        .is_ok()
    );
}

/// Build a real repo + linked worktree. Returns `None` when git is
/// unavailable so the test skips loudly rather than passing vacuously.
fn repo_with_worktree() -> Option<(tempfile::TempDir, PathBuf, PathBuf)> {
    let tmp = tempfile::tempdir().ok()?;
    let main = tmp.path().join("main");
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
    let wt = tmp.path().join("wt");
    if !git(&["worktree", "add", "-q", wt.to_str()?, "-b", "feat"]) {
        return None;
    }
    Some((tmp, main, wt))
}

/// Issue #004: granting the checkout must reach its worktrees. The user
/// reported having to authorise the same repo twice — once for the repo,
/// once for each worktree — because this gate is pure prefix matching and
/// a worktree lives outside the checkout it belongs to.
#[test]
fn a_worktree_of_a_granted_checkout_is_writable() {
    let Some((_tmp, main, wt)) = repo_with_worktree() else {
        eprintln!("skipping: git unavailable");
        return;
    };
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let fs = FilesystemEntitlement {
        read: vec![],
        // ONLY the main checkout is granted — exactly what the user typed.
        write: vec![main.to_string_lossy().into_owned()],
        deny: vec![],
    };
    let target = std::fs::canonicalize(&wt).unwrap().join("src/new.rs");
    assert!(
        check_write_entitlement(&fs, &target, &chain).is_ok(),
        "a worktree of the granted checkout must be writable without a second grant"
    );
}

/// The derivation must not become a general widening: an unrelated repo
/// is still refused, and so is an ordinary directory that merely sits
/// beside the grant.
#[test]
fn worktree_derivation_does_not_widen_to_unrelated_paths() {
    let Some((_tmp, main, _wt)) = repo_with_worktree() else {
        eprintln!("skipping: git unavailable");
        return;
    };
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let fs = FilesystemEntitlement {
        read: vec![],
        write: vec![main.to_string_lossy().into_owned()],
        deny: vec![],
    };
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = std::fs::canonicalize(elsewhere.path())
        .unwrap()
        .join("nope.rs");
    assert!(
        check_write_entitlement(&fs, &outside, &chain).is_err(),
        "a path outside every grant must stay refused"
    );
}

/// The safety boundary of #004: `deny` is evaluated first and stays
/// LITERAL. A denied path inside a derived worktree grant must still be
/// refused — the derivation is allow-side only, so it can never reopen
/// something the user explicitly closed.
#[test]
fn deny_still_wins_inside_a_derived_worktree_grant() {
    let Some((_tmp, main, wt)) = repo_with_worktree() else {
        eprintln!("skipping: git unavailable");
        return;
    };
    let chain = crate::sandbox::launch_chain::LaunchChain::inert();
    let wt_canon = std::fs::canonicalize(&wt).unwrap();
    let secrets = wt_canon.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();

    let fs = FilesystemEntitlement {
        read: vec![],
        write: vec![main.to_string_lossy().into_owned()],
        deny: vec![secrets.to_string_lossy().into_owned()],
    };
    assert!(
        check_write_entitlement(&fs, &secrets.join("key.pem"), &chain).is_err(),
        "deny must beat a derived worktree grant"
    );
    // ...while the rest of the same worktree remains writable.
    assert!(check_write_entitlement(&fs, &wt_canon.join("ok.rs"), &chain).is_ok());
}

// ── T2: typed ReadRefusal ───────────────────────────────────────────────────

/// A canonical tempdir holding a fake MUR home, with the launch chain rooted
/// at `agents/mur`. Canonical because the gate compares canonicalized paths
/// (macOS `/var` → `/private/var`).
fn refusal_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    crate::sandbox::launch_chain::LaunchChain,
) {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    for d in ["agents/mur", "proj/secret", "proj/open", "other", "wr"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
        &home.join("agents/mur"),
        &home.join("bin"),
        &home.join("home"),
    );
    (tmp, home, chain)
}

fn root(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn read_refusal_launch_chain_wins_over_deny_and_grant() {
    let (_tmp, home, chain) = refusal_fixture();
    let key = home.join("agents/mur/identity.key");
    let fs = FilesystemEntitlement {
        read: vec![root(&home)],
        write: vec![root(&home)],
        deny: vec![root(&home.join("agents"))],
    };
    assert_eq!(
        check_read_refusal(&fs, &key, &chain),
        Err(ReadRefusal::LaunchChain)
    );
}

#[test]
fn read_refusal_deny_list_wins_over_grant() {
    let (_tmp, home, chain) = refusal_fixture();
    let fs = FilesystemEntitlement {
        read: vec![root(&home.join("proj"))],
        deny: vec![root(&home.join("proj/secret"))],
        ..Default::default()
    };
    assert_eq!(
        check_read_refusal(&fs, &home.join("proj/secret/AGENTS.md"), &chain),
        Err(ReadRefusal::DenyList)
    );
    // Negative control: the same grant still reads a sibling.
    assert_eq!(
        check_read_refusal(&fs, &home.join("proj/open/AGENTS.md"), &chain),
        Ok(())
    );
}

#[test]
fn read_refusal_no_grant_when_outside_every_root() {
    let (_tmp, home, chain) = refusal_fixture();
    let fs = FilesystemEntitlement {
        read: vec![root(&home.join("proj"))],
        write: vec![root(&home.join("wr"))],
        ..Default::default()
    };
    assert_eq!(
        check_read_refusal(&fs, &home.join("other/AGENTS.md"), &chain),
        Err(ReadRefusal::NoGrant)
    );
}

#[test]
fn read_refusal_ok_under_read_or_write_grant() {
    let (_tmp, home, chain) = refusal_fixture();
    let fs = FilesystemEntitlement {
        read: vec![root(&home.join("proj"))],
        write: vec![root(&home.join("wr"))],
        ..Default::default()
    };
    assert_eq!(
        check_read_refusal(&fs, &home.join("proj/open/AGENTS.md"), &chain),
        Ok(())
    );
    assert_eq!(
        check_read_refusal(&fs, &home.join("wr/CLAUDE.md"), &chain),
        Ok(()),
        "write implies read-back"
    );
}

/// The string gate is now a presentation of the typed one. Expected values are
/// copied from the baseline `69938cd9:fs_policy.rs:322-340`, not from the new
/// code, so a drift in either direction fails here.
#[test]
fn check_read_entitlement_strings_are_unchanged() {
    let (_tmp, home, chain) = refusal_fixture();
    let fs = FilesystemEntitlement {
        read: vec![root(&home.join("proj")), root(&home.join("agents"))],
        deny: vec![root(&home.join("proj/secret"))],
        ..Default::default()
    };
    let msg = |p: &Path| match check_read_entitlement(&fs, p, &chain) {
        Err(ToolError::Execution(s)) => s,
        other => panic!(
            "expected Execution error for {}, got {other:?}",
            p.display()
        ),
    };

    let key = home.join("agents/mur/identity.key");
    assert_eq!(
        msg(&key),
        format!(
            "path is part of MUR's launch chain and can never be read: {} ({})",
            key.display(),
            "this agent's own signing key — reading it is enough to forge \
             its signed channel events"
        )
    );

    let denied = home.join("proj/secret/AGENTS.md");
    assert_eq!(
        msg(&denied),
        format!("path denied by entitlement: {}", denied.display())
    );

    let outside = home.join("other/AGENTS.md");
    assert_eq!(
        msg(&outside),
        format!(
            "path not entitled: {} (grant it via `mur agent perm allow-read`)",
            outside.display()
        )
    );

    assert!(check_read_entitlement(&fs, &home.join("proj/open/AGENTS.md"), &chain).is_ok());
}

/// Spec §5.1: the refusal carries no path and no error text — the only thing
/// the instructions block can ever learn from it is which of three reasons.
#[test]
fn read_refusal_is_a_bare_token() {
    fn assert_copy<T: Copy + Eq + std::fmt::Debug>() {}
    assert_copy::<ReadRefusal>();
    assert_eq!(std::mem::size_of::<ReadRefusal>(), 1);
}
