use super::*;
use chrono::{TimeZone, Utc};
use mur_common::identity::AgentIdentity;

fn rf() -> RequestFile {
    RequestFile {
        agent: "bob".into(),
        task_id: "t1".into(),
        request_id: "r1".into(),
        repo_id: "repo".into(),
        remote_id: "origin".into(),
        r#ref: "refs/heads/agent/x".into(),
        old_sha: "a".repeat(40),
        new_sha: "b".repeat(40),
        pack_sha256: "c".repeat(64),
        requested_at: Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap(),
        key_version: 0,
        sig: String::new(),
    }
}
fn signed(id: &AgentIdentity) -> RequestFile {
    let mut r = rf();
    r.sig = id.sign_multibase(&r.sign_input());
    r
}

#[test]
fn sign_input_excludes_sig_and_is_domain_tagged() {
    let mut a = rf();
    a.sig = "x".into();
    let mut b = rf();
    b.sig = "y".into();
    assert_eq!(
        a.sign_input(),
        b.sign_input(),
        "sig must not be part of what is signed"
    );
    assert!(a.sign_input().starts_with(b"mur-git-push-request-v1\n"));
}

#[test]
fn every_field_is_covered_by_the_signature() {
    let id = AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    let base = signed(&id);
    assert!(base.verify(&pk));
    #[allow(clippy::type_complexity)]
    let muts: Vec<Box<dyn Fn(&mut RequestFile)>> = vec![
        Box::new(|r| r.agent = "eve".into()),
        Box::new(|r| r.task_id = "t2".into()),
        Box::new(|r| r.request_id = "r2".into()),
        Box::new(|r| r.repo_id = "other".into()),
        Box::new(|r| r.remote_id = "other".into()),
        Box::new(|r| r.r#ref = "refs/heads/agent/y".into()),
        Box::new(|r| r.old_sha = "d".repeat(40)),
        Box::new(|r| r.new_sha = "d".repeat(40)),
        Box::new(|r| r.pack_sha256 = "d".repeat(64)),
        Box::new(|r| r.key_version = 1),
        Box::new(|r| r.requested_at = Utc.with_ymd_and_hms(2026, 10, 8, 0, 0, 0).unwrap()),
    ];
    for (i, m) in muts.iter().enumerate() {
        let mut r = base.clone();
        m(&mut r);
        assert!(!r.verify(&pk), "field #{i} not signed");
    }
}

/// Line framing: a value that moves a `\n` from one field into its neighbour
/// signs the same bytes, so a field containing a newline must never verify.
#[test]
fn a_newline_inside_a_field_fails_closed() {
    let id = AgentIdentity::generate();
    let mut r = rf();
    r.task_id = "t1\nr1".into();
    r.request_id = "repo".into();
    r.sig = id.sign_multibase(&r.sign_input());
    assert!(!r.verify(&id.verifying_key_bytes()));
}

#[test]
fn wrong_key_and_empty_sig_fail_closed() {
    let a = AgentIdentity::generate();
    let b = AgentIdentity::generate();
    assert!(!signed(&a).verify(&b.verifying_key_bytes()));
    assert!(!rf().verify(&a.verifying_key_bytes()), "empty sig");
}

#[test]
fn tampered_pack_digest_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    let pack = dir.path().join("pack.pack");
    std::fs::write(&pack, b"PACKdata").unwrap();
    let mut r = rf();
    r.pack_sha256 = sha256_file(&pack).unwrap();
    r.sig = id.sign_multibase(&r.sign_input());
    assert!(r.verify_pack(&pack));
    std::fs::write(&pack, b"PACKdatX").unwrap();
    assert!(!r.verify_pack(&pack), "pack changed after signing");
}

#[test]
fn agent_id_comes_from_the_verified_directory_not_the_file() {
    // alice's file, signed by bob, dropped in agents/bob/: the file says "alice" but the directory is bob's.
    let root = tempfile::tempdir().unwrap();
    let bob = AgentIdentity::generate();
    let mut r = rf();
    r.agent = "alice".into();
    r.sig = bob.sign_multibase(&r.sign_input());
    let inbox = root.path().join("agents/bob/inbox/git-push");
    std::fs::create_dir_all(&inbox).unwrap();
    write_request(&inbox, &r).unwrap();
    let got = verify_request(&inbox.join("r1.yaml"), "bob", &bob.verifying_key_bytes());
    assert!(
        matches!(got, Err(RequestFileError::AgentMismatch)),
        "{got:?}"
    );
}

#[test]
fn verified_request_reports_the_directory_agent() {
    let root = tempfile::tempdir().unwrap();
    let bob = AgentIdentity::generate();
    let r = signed(&bob);
    let inbox = root.path().join("agents/bob/inbox/git-push");
    std::fs::create_dir_all(&inbox).unwrap();
    write_request(&inbox, &r).unwrap();
    let ok = verify_request(&inbox.join("r1.yaml"), "bob", &bob.verifying_key_bytes()).unwrap();
    assert_eq!(ok.agent, "bob");
}

#[test]
fn a_renamed_request_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bob = AgentIdentity::generate();
    write_request(dir.path(), &signed(&bob)).unwrap();
    std::fs::rename(dir.path().join("r1.yaml"), dir.path().join("r9.yaml")).unwrap();
    let got = verify_request(
        &dir.path().join("r9.yaml"),
        "bob",
        &bob.verifying_key_bytes(),
    );
    assert_eq!(got, Err(RequestFileError::NameMismatch));
}

#[test]
fn path_traversal_in_ids_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let long = "x".repeat(MAX_ID_LEN + 1);
    for bad in ["../x", "a/b", "", ".", "..", "a\0b", long.as_str()] {
        let mut r = rf();
        r.request_id = bad.into();
        assert!(write_request(dir.path(), &r).is_err(), "request_id {bad:?}");
        let mut r = rf();
        r.agent = bad.into();
        assert!(write_request(dir.path(), &r).is_err(), "agent {bad:?}");
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing written"
    );
}

#[test]
fn write_is_atomic_and_idempotent_per_request_id() {
    let dir = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    let r = signed(&id);
    write_request(dir.path(), &r).unwrap();
    write_request(dir.path(), &r).unwrap();
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("r1.yaml")],
        "one file, no leftover .tmp"
    );
}

#[test]
fn same_id_with_different_content_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    write_request(dir.path(), &signed(&id)).unwrap();
    let mut other = signed(&id);
    other.new_sha = "e".repeat(40);
    assert_eq!(
        write_request(dir.path(), &other),
        Err(RequestFileError::Conflict)
    );
}
