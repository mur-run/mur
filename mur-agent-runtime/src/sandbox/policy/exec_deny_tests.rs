use super::*;

fn exe(path: &Path) {
    std::fs::write(path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A fake `~/.local/bin`: the CLI, its `murmur` alias, a runtime link and an
/// unrelated tool. Canonicalized so symlink-resolved forms compare equal on
/// macOS, where the temp dir lives under `/private`.
fn bin_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = std::fs::canonicalize(tmp.path()).unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("mur"));
    exe(&bin.join("mur-agent-runtime"));
    exe(&bin.join("jq"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(bin.join("mur"), bin.join("murmur")).unwrap();
        std::os::unix::fs::symlink(bin.join("mur-agent-runtime"), bin.join("mur_agent_pm"))
            .unwrap();
    }
    (tmp, bin)
}

#[test]
fn the_cli_in_an_exec_dir_is_denied_under_allowlist_and_strict() {
    let (_tmp, bin) = bin_dir();
    for mode in [SpawnMode::Allowlist, SpawnMode::Strict, SpawnMode::None] {
        let deny = mur_cli_exec_denies(mode, std::slice::from_ref(&bin), &[], None);
        assert!(deny.contains(&bin.join("mur")), "{mode:?}: {deny:?}");
        #[cfg(unix)]
        assert!(deny.contains(&bin.join("murmur")), "{mode:?}: {deny:?}");
        // Negative controls: the runtime, its agent links and unrelated tools
        // stay exec-able — the runtime re-execs itself after the seal.
        for keep in ["mur-agent-runtime", "mur_agent_pm", "jq"] {
            assert!(!deny.contains(&bin.join(keep)), "{mode:?} denied {keep}");
        }
    }
}

#[test]
fn any_mode_denies_nothing() {
    let (_tmp, bin) = bin_dir();
    assert!(mur_cli_exec_denies(SpawnMode::Any, &[bin], &[], None).is_empty());
}

#[test]
fn an_explicit_allowlist_entry_is_not_denied() {
    // A `browser` MCP entry runs `mur browser record`: the profile allowlisted
    // the exact file, and that consent must survive. The alias, which nobody
    // allowlisted, is still closed.
    let (_tmp, bin) = bin_dir();
    let deny = mur_cli_exec_denies(
        SpawnMode::Allowlist,
        std::slice::from_ref(&bin),
        &[bin.join("mur")],
        None,
    );
    assert!(!deny.contains(&bin.join("mur")), "{deny:?}");
    #[cfg(unix)]
    {
        // `murmur` resolves to the allowlisted file, so it is granted too:
        // the kernel only ever sees the resolved inode.
        assert!(!deny.contains(&bin.join("murmur")), "{deny:?}");
    }
}

#[test]
fn the_runtime_cli_path_is_covered_when_an_exec_dir_reaches_it() {
    // `extra` is the CLI the runtime itself would exec; named differently from
    // `mur`, it is still covered so long as a granted directory contains it.
    let (_tmp, bin) = bin_dir();
    let odd = bin.join("mur-cli-renamed");
    exe(&odd);
    let deny = mur_cli_exec_denies(
        SpawnMode::Allowlist,
        std::slice::from_ref(&bin),
        &[],
        Some(&odd),
    );
    assert!(deny.contains(&odd), "{deny:?}");

    // Outside every exec dir it is not this module's business: nothing grants it.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = std::fs::canonicalize(elsewhere.path()).unwrap().join("mur");
    exe(&outside);
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, &[bin], &[], Some(&outside));
    assert!(!deny.contains(&outside), "{deny:?}");
}

#[test]
fn a_missing_dir_or_absent_cli_yields_no_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let deny = mur_cli_exec_denies(
        SpawnMode::Allowlist,
        &[tmp.path().join("nope"), empty],
        &[],
        None,
    );
    assert!(deny.is_empty(), "{deny:?}");
}

#[test]
fn landlock_grants_split_only_the_dir_holding_a_denied_file() {
    let (_tmp, bin) = bin_dir();
    let other = bin.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let fs_exec = vec![bin.clone(), other.clone()];
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, &fs_exec, &[], None);

    let grants = exec_grants_without(&fs_exec, &deny);
    // The untouched directory is still one directory-wide grant.
    assert!(grants.contains(&other), "{grants:?}");
    // The CLI directory is no longer granted whole …
    assert!(!grants.contains(&bin), "{grants:?}");
    // … the CLI and every link resolving to it are gone …
    assert!(!grants.contains(&bin.join("mur")), "{grants:?}");
    #[cfg(unix)]
    assert!(!grants.contains(&bin.join("murmur")), "{grants:?}");
    // … and its siblings survive as individual grants.
    for keep in ["jq", "mur-agent-runtime"] {
        assert!(grants.contains(&bin.join(keep)), "lost {keep}: {grants:?}");
    }
    #[cfg(unix)]
    assert!(grants.contains(&bin.join("mur_agent_pm")), "{grants:?}");
}

#[test]
fn landlock_grants_are_unchanged_without_denies() {
    let (_tmp, bin) = bin_dir();
    assert_eq!(
        exec_grants_without(std::slice::from_ref(&bin), &[]),
        vec![bin]
    );
}
