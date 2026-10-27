//! `write_protected_paths()` and the path table it implements (spec
//! 2026-10-25 "One path table"). Each list is checked in BOTH directions
//! against the table, so adding or removing a row fails until the table here
//! and the code change together.

use super::*;
use std::collections::BTreeSet;

struct Fixture {
    _tmp: tempfile::TempDir,
    mur: PathBuf,
    home: PathBuf,
    chain: LaunchChain,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path().join("mur");
    let home = tmp.path().join("home");
    let chain = LaunchChain::for_test(&mur.join("agents").join("alice"), &mur.join("bin"), &home);
    Fixture {
        _tmp: tmp,
        mur,
        home,
        chain,
    }
}

impl Fixture {
    /// The credential rows: "Read denied = yes" and "Write denied = yes".
    fn credential_rows(&self) -> Vec<PathBuf> {
        let m = &self.mur;
        let mut v = vec![
            m.join("keys"),
            m.join("secrets"),
            m.join("auth.json"),
            m.join("identity.key"),
            m.join("commander").join("identity.key"),
            m.join("commander").join("signing.key"),
            m.join("commander").join(".env"),
            m.join("publisher").join("identity.key"),
            m.join("mobile").join("pair-token"),
            m.join("runtime").join("vlc.json"),
            m.join("actions-runner"),
        ];
        v.extend([".ssh", ".aws", ".gnupg"].map(|d| self.home.join(d)));
        v
    }

    /// Write denied only: readable trust anchors.
    fn commander_public_rows(&self) -> Vec<PathBuf> {
        let c = self.mur.join("commander");
        vec![c.join("identity.pub"), c.join("identity.prev.pub")]
    }
}

fn set(v: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    v.into_iter().collect()
}

#[test]
fn write_protected_paths_equals_the_write_rows_of_the_table() {
    let f = fixture();
    let mut expected = f.chain.deny_paths();
    expected.extend(f.credential_rows());
    expected.extend(f.commander_public_rows());
    assert_eq!(set(f.chain.write_protected_paths()), set(expected));
}

#[test]
fn credential_paths_equals_the_read_rows_of_the_table() {
    let f = fixture();
    let mut expected = f.credential_rows();
    expected.extend(CAPTURE_STORES.iter().map(|s| f.mur.join(s)));
    assert_eq!(set(f.chain.credential_paths()), set(expected));
}

#[test]
fn capture_store_paths_come_from_the_one_name_list() {
    let f = fixture();
    let expected: Vec<PathBuf> = CAPTURE_STORES.iter().map(|s| f.mur.join(s)).collect();
    assert_eq!(f.chain.capture_store_paths(), expected);
    for p in &expected {
        assert!(f.chain.protects_read(p).is_some(), "{}", p.display());
        assert!(f.chain.protects_write(p).is_some(), "{}", p.display());
    }
}

/// Writing a recording grants nothing, and the fleet runner's own
/// `conversations/` grant must survive the Landlock partition.
#[test]
fn the_write_list_excludes_capture_stores_and_the_publisher_public_key() {
    let f = fixture();
    let w = f.chain.write_protected_paths();
    for p in f.chain.capture_store_paths() {
        assert!(
            !w.iter().any(|x| x.starts_with(&p) || p.starts_with(x)),
            "{} overlaps the write list",
            p.display()
        );
    }
    assert!(!w.contains(&f.mur.join("publisher").join("identity.pub")));
    assert!(!w.contains(&f.mur.join("publisher")));
}

#[test]
fn the_startup_override_store_is_write_protected() {
    let f = fixture();
    let ovr = f
        .mur
        .join("secrets")
        .join("startup-overrides")
        .join("alice.json");
    assert!(
        f.chain
            .write_protected_paths()
            .iter()
            .any(|p| ovr.starts_with(p))
    );
    let (_, dropped) = f.chain.partition_grants(std::slice::from_ref(&ovr));
    assert_eq!(dropped, vec![ovr]);
}

/// N1/R5-3: the pinned commander keys are trust anchors. Readable, so the
/// fleet loop is governed; never writable, so no agent pins its own key.
#[test]
fn commander_public_keys_are_readable_but_never_writable() {
    let f = fixture();
    for p in f.commander_public_rows() {
        assert!(f.chain.protects_write(&p).is_some(), "{}", p.display());
        assert!(f.chain.protects_read(&p).is_none(), "{}", p.display());
        let (_, dropped) = f.chain.partition_grants(std::slice::from_ref(&p));
        assert_eq!(dropped, vec![p]);
    }
}

/// N2: the live commander key is `identity.key`; `signing.key` is the legacy
/// name. Both are covered by every list and predicate.
#[test]
fn both_commander_private_key_names_are_covered_everywhere() {
    let f = fixture();
    let c = f.mur.join("commander");
    for p in [c.join("identity.key"), c.join("signing.key")] {
        assert!(f.chain.protects_read(&p).is_some(), "{}", p.display());
        assert!(f.chain.protects_write(&p).is_some(), "{}", p.display());
        assert!(f.chain.credential_paths().contains(&p));
        assert!(f.chain.write_protected_paths().contains(&p));
    }
    // A grant on the directory reaches both, so Landlock drops it whole.
    let (kept, dropped) = f.chain.partition_grants(std::slice::from_ref(&c));
    assert!(kept.is_empty());
    assert_eq!(dropped, vec![c]);
}

/// R5-5: the publisher key lives at `publisher/identity.key`. The
/// pre-#1011 name `publisher-identity.key` was never created and is in no list.
#[test]
fn the_publisher_private_key_is_covered_and_the_stale_name_is_not() {
    let f = fixture();
    let key = f.mur.join("publisher").join("identity.key");
    assert!(f.chain.protects_read(&key).is_some());
    assert!(f.chain.protects_write(&key).is_some());
    assert!(f.chain.credential_paths().contains(&key));
    assert!(f.chain.write_protected_paths().contains(&key));
    let stale = |v: Vec<PathBuf>| {
        v.iter()
            .any(|p| p.to_string_lossy().contains("publisher-identity.key"))
    };
    assert!(!stale(f.chain.credential_paths()));
    assert!(!stale(f.chain.write_protected_paths()));
}

/// The write twin of `a_read_grant_containing_the_credential_store_is_dropped_not_carved`.
#[test]
fn partition_drops_credential_grants_and_keeps_run_state() {
    let f = fixture();
    let m = &f.mur;
    let dropped_expected = vec![m.join("secrets"), m.clone()];
    let kept_expected = vec![
        m.join("conversations"),
        m.join("artifacts"),
        m.join("runs"),
        mur_common::paths::fleet_state_dir(m, "deep-research"),
        m.join("channels"),
        m.join("index").join("channels"),
        m.join(mur_open_items::LOG_FILE),
        m.join(mur_track::fleet_dispatch_request::FLEET_DISPATCH_REQUEST_DIR),
    ];
    let mut grants = dropped_expected.clone();
    grants.extend(kept_expected.clone());
    let (kept, dropped) = f.chain.partition_grants(&grants);
    assert_eq!(dropped, dropped_expected);
    assert_eq!(kept, kept_expected);
}

/// A grant named by a symlink is judged by where it lands, not how it is
/// spelled: the builder keeps the link next to its target and Landlock
/// resolves it at install time, so a lexical test lets it through.
#[cfg(unix)]
#[test]
fn a_symlinked_grant_into_a_protected_path_is_dropped() {
    let f = fixture();
    let secrets = f.mur.join("secrets");
    std::fs::create_dir_all(secrets.join("startup-overrides")).unwrap();
    let work = f.home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let link = work.join("granted-cache");
    std::os::unix::fs::symlink(&secrets, &link).unwrap();
    let plain = work.join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    let grants = vec![link.clone(), plain.clone()];
    let (kept, dropped) = f.chain.partition_grants(&grants);
    assert_eq!(dropped, vec![link.clone()]);
    assert_eq!(kept, vec![plain.clone()]);
    let (kept_r, dropped_r) = f.chain.partition_read_grants(&grants);
    assert_eq!(dropped_r, vec![link]);
    assert_eq!(kept_r, vec![plain]);
}

/// The own-home exemption must not carry a protected target: an alias
/// placed inside the agent's home still resolves outside it.
#[cfg(unix)]
#[test]
fn an_alias_inside_the_own_home_does_not_bypass_the_check() {
    let f = fixture();
    let secrets = f.mur.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let own = f.chain.agent_self_home().to_path_buf();
    std::fs::create_dir_all(&own).unwrap();
    let link = own.join("vault");
    std::os::unix::fs::symlink(&secrets, &link).unwrap();
    let state = own.join("state");
    std::fs::create_dir_all(&state).unwrap();

    let (kept, dropped) = f
        .chain
        .partition_grants(&[own.clone(), link.clone(), state.clone()]);
    assert_eq!(dropped, vec![link]);
    assert_eq!(kept, vec![own, state]);
}

/// A symlink to a protected path's ANCESTOR (here the whole `<mur_home>`)
/// reaches it too and is dropped.
#[cfg(unix)]
#[test]
fn a_symlink_to_an_ancestor_of_a_protected_path_is_dropped() {
    let f = fixture();
    std::fs::create_dir_all(f.mur.join("secrets")).unwrap();
    std::fs::create_dir_all(&f.home).unwrap();
    let link = f.home.join("mur-alias");
    std::os::unix::fs::symlink(&f.mur, &link).unwrap();
    let (kept, dropped) = f.chain.partition_grants(std::slice::from_ref(&link));
    assert!(kept.is_empty());
    assert_eq!(dropped, vec![link]);
}
