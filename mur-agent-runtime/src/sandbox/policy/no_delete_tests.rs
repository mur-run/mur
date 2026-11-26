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
