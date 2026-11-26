use super::*;
use std::fs;

fn tree() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    for d in [
        "work/src",
        "work/data/reports/2026",
        "work/data/cache",
        "other",
    ] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    fs::write(root.join("work/README"), b"x").unwrap();
    (t, root)
}

#[test]
fn no_entries_leaves_every_grant_whole() {
    let (_t, r) = tree();
    let g = vec![r.join("work"), r.join("other")];
    let s = split_no_delete_grants(&g, &[]);
    assert_eq!(s.full, g);
    assert!(s.no_remove.is_empty());
}

#[test]
fn a_grant_above_a_no_delete_path_is_carved_down_to_it() {
    let (_t, r) = tree();
    let nd = vec![r.join("work/data/reports")];
    let s = split_no_delete_grants(&[r.join("work"), r.join("other")], &nd);

    // Removal stripped on the grant root, the directory on the way down, and
    // the protected path itself.
    for p in ["work", "work/data", "work/data/reports"] {
        assert!(s.no_remove.contains(&r.join(p)), "{p} must lose Remove*");
    }
    // Siblings off the path keep everything, as does an unrelated grant.
    for p in ["work/src", "work/README", "work/data/cache", "other"] {
        assert!(s.full.contains(&r.join(p)), "{p} must keep full rights");
    }
    // Nothing with removal rights covers the protected tree.
    for f in &s.full {
        assert!(
            !nd[0].starts_with(f) && !f.starts_with(&nd[0]),
            "full grant {} reaches the no-delete path",
            f.display()
        );
    }
}

#[test]
fn a_grant_inside_a_no_delete_path_loses_removal_whole() {
    let (_t, r) = tree();
    let nd = vec![r.join("work/data")];
    let g = vec![r.join("work/data/cache")];
    let s = split_no_delete_grants(&g, &nd);
    assert_eq!(s.no_remove, g);
    assert!(s.full.is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlinked_grant_is_judged_by_its_target() {
    let (_t, r) = tree();
    let link = r.join("link");
    std::os::unix::fs::symlink(r.join("work"), &link).unwrap();
    let s = split_no_delete_grants(std::slice::from_ref(&link), &[r.join("work/data/reports")]);
    assert!(s.no_remove.contains(&link), "the link reaches the tree too");
    assert!(!s.full.contains(&link));
}

#[test]
fn an_entry_outside_every_write_grant_is_reported_ineffective() {
    let (_t, r) = tree();
    let p = SandboxPolicy {
        fs_write: vec![r.join("work")],
        fs_no_delete: vec![r.join("work/data"), r.join("other")],
        ..Default::default()
    };
    assert_eq!(p.ineffective_no_delete(), vec![r.join("other")]);
}

#[test]
fn an_empty_list_always_passes_the_launch_check() {
    assert!(
        SandboxPolicy::default()
            .check_no_delete_enforceable()
            .is_ok()
    );
}

#[test]
fn the_entitlement_round_trips_and_defaults_empty() {
    let fs: mur_common::agent::FilesystemEntitlement =
        serde_yaml_ng::from_str("{read: [], write: [], deny: []}").unwrap();
    assert!(fs.no_delete.is_empty());
    let out = serde_yaml_ng::to_string(&fs).unwrap();
    assert!(!out.contains("no_delete"), "empty list must not be written");

    let fs: mur_common::agent::FilesystemEntitlement =
        serde_yaml_ng::from_str("{write: [/w], no_delete: [/w/keep]}").unwrap();
    assert_eq!(fs.no_delete, vec!["/w/keep".to_string()]);
}

/// Review P1 on #2595: a missing entry named through a symlinked parent must
/// still reach the real tree, or the kernel rule lands on a name nothing
/// resolves to and the directory is deletable once it appears.
#[cfg(unix)]
#[test]
fn a_missing_entry_behind_a_symlinked_parent_resolves_to_the_real_tree() {
    let (_t, r) = tree();
    let alias = r.join("alias");
    std::os::unix::fs::symlink(r.join("work"), &alias).unwrap();
    let entry = alias.join("future/deeper");
    let nd = resolve_no_delete(vec![entry.clone()]);
    assert!(nd.contains(&entry), "the name as written is kept");
    assert!(
        nd.contains(&r.join("work/future/deeper")),
        "the real path is added: {nd:?}"
    );
    let s = split_no_delete_grants(&[r.join("work")], &nd);
    assert!(s.no_remove.contains(&r.join("work")));
    assert!(!s.full.contains(&r.join("work")));
}

#[test]
fn a_parent_dir_component_is_refused_at_launch() {
    let p = SandboxPolicy {
        fs_no_delete: vec![PathBuf::from("/w/missing/../keep")],
        ..Default::default()
    };
    let e = p.check_no_delete_enforceable().unwrap_err().to_string();
    assert!(e.contains(".."), "{e}");
}

/// Review P1 on #2595 (second round): a DANGLING symlink ancestor must be
/// followed to its target, not appended as text — otherwise the entry never
/// overlaps the write grant, the grant keeps removal rights, and the target
/// is deletable once created after the seal.
#[cfg(unix)]
#[test]
fn a_dangling_symlink_ancestor_resolves_to_its_future_target() {
    let (_t, r) = tree();
    let alias = r.join("alias");
    std::os::unix::fs::symlink(r.join("work/new-root"), &alias).unwrap();
    assert!(!alias.exists(), "precondition: the link dangles");
    let entry = alias.join("keep");
    let nd = resolve_no_delete(vec![entry.clone()]);
    assert!(nd.contains(&entry), "the name as written is kept");
    assert!(
        nd.contains(&r.join("work/new-root/keep")),
        "the link target is followed: {nd:?}"
    );
    let s = split_no_delete_grants(&[r.join("work")], &nd);
    assert!(s.no_remove.contains(&r.join("work")));
    assert!(!s.full.contains(&r.join("work")));
}

/// A relative dangling target resolves against the link's own directory,
/// and chained links are followed hop by hop.
#[cfg(unix)]
#[test]
fn chained_and_relative_dangling_links_are_followed() {
    let (_t, r) = tree();
    std::os::unix::fs::symlink("work/next", r.join("a")).unwrap();
    std::os::unix::fs::symlink("../later", r.join("work/next")).unwrap();
    let nd = resolve_no_delete(vec![r.join("a/keep")]);
    assert!(nd.contains(&r.join("later/keep")), "{nd:?}");
}

/// A symlink loop has no answer; launch refuses instead of emitting a rule
/// on a name the kernel will never resolve.
#[cfg(unix)]
#[test]
fn a_symlink_loop_is_refused_at_launch() {
    let (_t, r) = tree();
    std::os::unix::fs::symlink(r.join("loop2"), r.join("loop1")).unwrap();
    std::os::unix::fs::symlink(r.join("loop1"), r.join("loop2")).unwrap();
    let entry = r.join("loop1/keep");
    let p = SandboxPolicy {
        fs_no_delete: resolve_no_delete(vec![entry]),
        ..Default::default()
    };
    let e = p.check_no_delete_enforceable().unwrap_err().to_string();
    assert!(e.contains("cannot be resolved"), "{e}");
}
