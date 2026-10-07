// tests/private_repo.rs — real git. `T` = Duration::from_secs(DEFAULT_GIT_TIMEOUT_SECS).
mod common;
use mur_git_broker::{constants::*, error::BrokerError, oid::ObjectFormat, repo::PrivateRepo};
use std::{fs, path::Path};

fn fresh() -> (tempfile::TempDir, PrivateRepo) {
    let t = tempfile::tempdir().unwrap();
    let r = PrivateRepo::create(t.path(), ObjectFormat::Sha1, &common::git_bin()).unwrap();
    (t, r)
}
fn entries(p: &Path) -> usize {
    fs::read_dir(p).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn fresh_repo_passes_inspection_and_has_empty_hooks() {
    let (_t, r) = fresh();
    r.inspect_forbidden().unwrap();
    assert_eq!(
        entries(&r.path().join("hooks")),
        0,
        "git's sample hooks must be gone"
    );
    assert!(!r.path().join("info").exists());
}
#[test]
fn each_forbidden_path_is_rejected_not_removed() {
    for p in FORBIDDEN_REPO_PATHS {
        let (_t, r) = fresh();
        let f = r.path().join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, b"x").unwrap();
        assert!(
            matches!(
                r.inspect_forbidden(),
                Err(BrokerError::AncestryUnprovable(_))
            ),
            "{p}"
        );
        assert!(f.exists(), "{p} must be reported, never silently deleted");
    }
}
#[test]
fn unknown_extension_is_rejected() {
    let (_t, r) = fresh();
    let c = r.path().join("config");
    let mut s = fs::read_to_string(&c).unwrap();
    s.push_str("[extensions]\n\tworktreeconfig = true\n");
    fs::write(&c, s).unwrap();
    assert!(matches!(
        r.inspect_forbidden(),
        Err(BrokerError::AncestryUnprovable(_))
    ));
}
#[test]
fn repository_format_version_2_is_rejected() {
    let (_t, r) = fresh();
    let c = r.path().join("config");
    let s = fs::read_to_string(&c)
        .unwrap()
        .replace("repositoryformatversion = 0", "repositoryformatversion = 2");
    fs::write(&c, s).unwrap();
    assert!(matches!(
        r.inspect_forbidden(),
        Err(BrokerError::AncestryUnprovable(_))
    ));
}
#[test]
fn sha256_repo_is_version_1_with_objectformat_extension() {
    let t = tempfile::tempdir().unwrap();
    let r = PrivateRepo::create(t.path(), ObjectFormat::Sha256, &common::git_bin()).unwrap();
    let cfg = fs::read_to_string(r.path().join("config")).unwrap();
    assert!(cfg.contains("repositoryformatversion = 1") && cfg.contains("objectformat = sha256"));
    r.inspect_forbidden().unwrap();
}
#[test]
fn control_digest_changes_on_config_hooks_and_refs() {
    type Mut = fn(&Path);
    let muts: [(&str, Mut); 3] = [
        ("config", |p| {
            let c = p.join("config");
            let mut s = fs::read_to_string(&c).unwrap();
            s.push_str("[x]\n\ty = 1\n");
            fs::write(c, s).unwrap();
        }),
        ("hook", |p| {
            fs::write(p.join("hooks/pre-push"), b"#!/bin/sh\n").unwrap()
        }),
        ("ref", |p| {
            fs::create_dir_all(p.join("refs/heads")).unwrap();
            fs::write(p.join("refs/heads/m"), "a".repeat(40)).unwrap();
        }),
    ];
    for (name, m) in muts {
        let (_t, r) = fresh();
        let before = r.control_digest().unwrap();
        m(r.path());
        assert_ne!(before, r.control_digest().unwrap(), "{name}");
    }
}
#[test]
fn digest_ignores_objects_pack() {
    let (_t, r) = fresh();
    let before = r.control_digest().unwrap();
    fs::write(r.path().join("objects/pack/pack.pack"), b"not a pack").unwrap();
    assert_eq!(before, r.control_digest().unwrap());
}
#[test]
fn symlink_in_a_control_path_is_an_error() {
    let (_t, r) = fresh();
    std::os::unix::fs::symlink("/etc/passwd", r.path().join("hooks/evil")).unwrap();
    assert!(r.control_digest().is_err());
}
#[test]
fn freeze_makes_control_files_read_only() {
    let (_t, r) = fresh();
    let d = r.freeze().unwrap();
    assert_eq!(
        d,
        r.control_digest().unwrap(),
        "freeze returns the digest of what it froze"
    );
    let e = fs::write(r.path().join("config"), b"x").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        fs::write(r.path().join("hooks/pre-push"), b"x").is_err(),
        "hooks/ dir is read-only too"
    );
}
#[test]
fn destroy_removes_everything_even_when_frozen() {
    let (_t, r) = fresh();
    r.freeze().unwrap();
    let p = r.path().to_path_buf();
    r.destroy();
    assert!(!p.exists());
}
