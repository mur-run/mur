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

    let split = split_exec_grants(&fs_exec, &deny);
    // The untouched directory is still one directory-wide grant.
    assert_eq!(split.clean, vec![other], "{split:?}");
    // The CLI directory keeps its non-exec rights whole …
    assert_eq!(split.no_exec, vec![bin.clone()], "{split:?}");
    let exec = &split.exec_only;
    // … the CLI and every link resolving to it get no Execute …
    assert!(!exec.contains(&bin.join("mur")), "{exec:?}");
    #[cfg(unix)]
    assert!(!exec.contains(&bin.join("murmur")), "{exec:?}");
    // … and its siblings keep Execute as individual grants.
    for keep in ["jq", "mur-agent-runtime"] {
        assert!(exec.contains(&bin.join(keep)), "lost {keep}: {exec:?}");
    }
    #[cfg(unix)]
    assert!(exec.contains(&bin.join("mur_agent_pm")), "{exec:?}");
}

#[test]
fn landlock_grants_are_unchanged_without_denies() {
    let (_tmp, bin) = bin_dir();
    let split = split_exec_grants(std::slice::from_ref(&bin), &[]);
    assert_eq!(split.clean, vec![bin]);
    assert!(split.no_exec.is_empty() && split.exec_only.is_empty());
}

/// No `exec_only` grant may reach a denied file: neither name it, nor be an
/// ancestor of it in written or resolved form. This is the property Landlock
/// needs — any such rule would hand `Execute` back recursively.
fn assert_no_exec_grant_reaches(split: &ExecSplit, deny: &[PathBuf]) {
    for g in split.clean.iter().chain(&split.exec_only) {
        assert!(
            !reaches_denied(g, deny),
            "{g:?} reaches a denied file: {split:?}"
        );
    }
}

#[test]
fn an_ancestor_grant_is_carved_recursively_not_kept_whole() {
    // `/tools` and `/tools/bin` both granted, CLI in `/tools/bin`: splitting
    // only the immediate parent would leave `/tools` granting it recursively.
    let (_tmp, bin) = bin_dir();
    let tools = bin.parent().unwrap().to_path_buf();
    let sibling = tools.join("lib");
    std::fs::create_dir_all(&sibling).unwrap();
    let grants = vec![tools.clone(), bin.clone()];
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, &grants, &[], None);
    assert!(deny.contains(&bin.join("mur")), "{deny:?}");

    let split = split_exec_grants(&grants, &deny);
    assert!(split.clean.is_empty(), "{split:?}");
    assert_eq!(split.no_exec, grants, "{split:?}");
    assert_no_exec_grant_reaches(&split, &deny);
    // Positive control: the carve still reaches past the ancestor.
    assert!(split.exec_only.contains(&sibling), "{split:?}");
    assert!(split.exec_only.contains(&bin.join("jq")), "{split:?}");
}

#[cfg(unix)]
#[test]
fn a_directory_symlink_to_the_cli_dir_is_carved_through() {
    // A rule installed ON a symlink follows it: Landlock opens the path and
    // binds the rule to the target directory, recursively. That happens when
    // the link is the grant itself, or an entry a carve turns into a grant
    // (`root/alias → bin` while carving `root`). Both must be carved through.
    // (A link merely nested inside a kept-whole grant is harmless: Landlock
    // checks the resolved path's ancestors, so it never reaches `bin`.)
    let (_tmp, bin) = bin_dir();
    let root = bin.parent().unwrap().to_path_buf();
    std::os::unix::fs::symlink(&bin, root.join("alias")).unwrap();
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, std::slice::from_ref(&bin), &[], None);

    for grant in [root.clone(), root.join("alias")] {
        let split = split_exec_grants(std::slice::from_ref(&grant), &deny);
        assert_eq!(split.no_exec, vec![grant.clone()], "{split:?}");
        assert!(!split.exec_only.contains(&root.join("alias")), "{split:?}");
        assert_no_exec_grant_reaches(&split, &deny);
        // Positive control: the carve went through the link, not around it.
        assert!(
            split.exec_only.contains(&root.join("alias").join("jq")),
            "{split:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_loop_back_to_a_carved_dir_is_dropped_not_granted() {
    // `bin/up → ..`: following it re-enters the carved ancestor; granting it
    // would grant that ancestor whole. The carve must stop, not loop.
    let (_tmp, bin) = bin_dir();
    let tools = bin.parent().unwrap().to_path_buf();
    std::os::unix::fs::symlink(&tools, bin.join("up")).unwrap();
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, std::slice::from_ref(&bin), &[], None);

    let split = split_exec_grants(std::slice::from_ref(&tools), &deny);
    assert!(!split.exec_only.contains(&bin.join("up")), "{split:?}");
    assert_no_exec_grant_reaches(&split, &deny);
}

#[test]
fn read_and_write_grants_reach_the_cli_on_landlock() {
    // Landlock's read and write rights include `Execute`. A CLI denied via its
    // exec dir must also lose Execute under an overlapping read grant (`~`)
    // and write grant (`~/.local`), or those rules hand it straight back.
    let (_tmp, bin) = bin_dir();
    let home = bin.parent().unwrap().to_path_buf();
    let fs_exec = vec![bin.clone()];
    let fs_read = vec![home.clone()];
    let fs_write = vec![bin.clone()];
    let reach: Vec<PathBuf> = [&fs_exec, &fs_read, &fs_write]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, &reach, &[], None);

    for (what, grants) in [("read", &fs_read), ("write", &fs_write), ("exec", &fs_exec)] {
        let split = split_exec_grants(grants, &deny);
        assert_eq!(&split.no_exec, grants, "{what}: {split:?}");
        assert_no_exec_grant_reaches(&split, &deny);
    }

    // `extra` (the runtime's own CLI path) deeper than a read grant's entries
    // is reached by the read grant alone — no exec dir needs to hold it.
    let deep = home.join("cargo").join("bin");
    std::fs::create_dir_all(&deep).unwrap();
    exe(&deep.join("mur"));
    let deny = mur_cli_exec_denies(SpawnMode::Allowlist, &fs_read, &[], Some(&deep.join("mur")));
    assert!(deny.contains(&deep.join("mur")), "{deny:?}");
    assert_no_exec_grant_reaches(&split_exec_grants(&fs_read, &deny), &deny);
}
