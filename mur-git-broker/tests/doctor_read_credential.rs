#![cfg(unix)]
//! The doctor's view of a read credential (spec §6, §7.8, §7.12): the read secret is checked
//! exactly like the write secret, two names for one token are refused, a shared credential is
//! advice only, and no check prints a secret.
use mur_git_broker::constants::ALLOWED_REF_PREFIX;
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::doctor::{
    AgentFacts, EnrollmentProblem, check_enrollment, credential_problems,
};
use mur_git_broker::enrollment::Endpoint;
use mur_git_broker::policy::RemotePolicy;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const HTTPS: &str = "https://git.example.com/o/r.git";
/// Two values of the same length that differ in one byte, and one of a different length.
const TOKEN: &str = "tok-AAAAAAAAAAAA";
const TOKEN_ONE_BYTE_OFF: &str = "tok-AAAAAAAAAAAB";
const TOKEN_LONGER: &str = "tok-AAAAAAAAAAAAB";

struct Fx {
    _t: tempfile::TempDir,
    root: PathBuf,
}
fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(t.path()).unwrap();
    let root = base.join("secrets");
    std::fs::create_dir_all(&root).unwrap();
    Fx { _t: t, root }
}
fn put(root: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
    let f = root.join(name);
    std::fs::write(&f, body).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
    f
}
fn cred(p: &Path, user: &str) -> Credential {
    Credential {
        secret_path: SecretPath::parse(p.to_str().unwrap()).unwrap(),
        username: Username::parse(user).unwrap(),
    }
}
fn policy(endpoint: &str, write: &Path, read: Option<&Path>) -> RemotePolicy {
    RemotePolicy {
        remote_id: "r".into(),
        canonical_remote_endpoint: endpoint.into(),
        creation_base_refs: vec!["refs/heads/main".into()],
        ref_prefix: ALLOWED_REF_PREFIX.into(),
        credential: Some(cred(write, "bot")),
        read_credential: read.map(|p| cred(p, "reader")),
    }
}
fn agent(home: &Path, writes: &[&Path]) -> AgentFacts {
    AgentFacts {
        name: "a".into(),
        home: home.into(),
        write_paths: writes.iter().map(|p| p.to_path_buf()).collect(),
        fail_closed: true,
        sandbox_enforcing: Some(true),
    }
}
fn same_secret(problems: &[EnrollmentProblem]) -> bool {
    problems.contains(&EnrollmentProblem::CredentialSameSecret)
}

/// Spec §7.12: every way of naming one token twice is reported, each as a refusal.
#[test]
fn the_same_token_under_two_names_is_reported_as_the_same_secret() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let link = f.root.join("sym");
    std::os::unix::fs::symlink(&write, &link).unwrap();
    let hard = f.root.join("hard");
    std::fs::hard_link(&write, &hard).unwrap();
    let twin = put(&f.root, "twin", TOKEN, 0o600);
    // A trailing newline is not part of the token (the broker trims one), so it is still equal.
    let twin_nl = put(&f.root, "twin-nl", &format!("{TOKEN}\n"), 0o600);
    for (what, read) in [
        ("same path", &write),
        ("symlink", &link),
        ("hard link", &hard),
        ("equal contents", &twin),
        ("equal contents, trailing newline", &twin_nl),
    ] {
        let p = policy(HTTPS, &write, Some(read));
        let got = credential_problems(&p, &f.root, &[]);
        assert_eq!(got, vec![EnrollmentProblem::CredentialSameSecret], "{what}");
        assert!(got[0].is_refusal(), "{what}");
    }
}

#[test]
fn different_secrets_are_not_the_same_including_one_byte_and_length_differences() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let off_by_one = put(&f.root, "one-off", TOKEN_ONE_BYTE_OFF, 0o600);
    let longer = put(&f.root, "longer", TOKEN_LONGER, 0o600);
    let shorter = put(&f.root, "shorter", "tok-A", 0o600);
    for read in [&off_by_one, &longer, &shorter] {
        let p = policy(HTTPS, &write, Some(read));
        assert_eq!(credential_problems(&p, &f.root, &[]), vec![], "{read:?}");
    }
}

/// Spec §6 order of checks: an unusable secret is never reported as the same secret.
#[test]
fn an_unusable_secret_is_unusable_never_the_same_secret() {
    let f = fx();
    let good = put(&f.root, "good", TOKEN, 0o600);
    let gone = f.root.join("absent");
    let loose = put(&f.root, "loose", TOKEN, 0o644);
    let loose_link = f.root.join("loose-link");
    std::os::unix::fs::symlink(&loose, &loose_link).unwrap();
    let loose_hard = f.root.join("loose-hard");
    std::fs::hard_link(&loose, &loose_hard).unwrap();

    // Read secret unusable, write fine.
    for (read, why) in [(&gone, "not_found"), (&loose, "bad_mode")] {
        let got = credential_problems(&policy(HTTPS, &good, Some(read)), &f.root, &[]);
        assert_eq!(got, vec![EnrollmentProblem::CredentialUnusable(why)]);
        assert!(got[0].is_refusal());
    }
    // Write secret unusable, read fine.
    let got = credential_problems(&policy(HTTPS, &loose, Some(&good)), &f.root, &[]);
    assert_eq!(got, vec![EnrollmentProblem::CredentialUnusable("bad_mode")]);
    // Read is a symlink or a hard link to an unreadable write secret: both opens fail, each
    // reports itself, and the pair is not compared.
    for read in [&loose_link, &loose_hard] {
        let got = credential_problems(&policy(HTTPS, &loose, Some(read)), &f.root, &[]);
        assert_eq!(
            got,
            vec![
                EnrollmentProblem::CredentialUnusable("bad_mode"),
                EnrollmentProblem::CredentialUnusable("bad_mode"),
            ],
            "{read:?}"
        );
    }
}

/// The agent-writable check is per secrets root: a read credential does not repeat it.
#[test]
fn the_secrets_writable_by_agent_check_is_not_duplicated_for_the_read_secret() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let read = put(&f.root, "read", TOKEN_ONE_BYTE_OFF, 0o600);
    let home = f.root.parent().unwrap().join("home");
    let got = credential_problems(
        &policy(HTTPS, &write, Some(&read)),
        &f.root,
        &[agent(&home, &[&f.root])],
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(matches!(
        got[0],
        EnrollmentProblem::SecretsWritableByAgent { .. }
    ));
}

/// Spec §7.12: no problem, however it is shown, carries a secret value.
#[test]
fn no_problem_text_contains_a_secret_value() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let twin = put(&f.root, "twin", TOKEN, 0o600);
    let loose = put(&f.root, "loose", TOKEN_LONGER, 0o644);
    for read in [&twin, &loose] {
        let got = credential_problems(&policy(HTTPS, &write, Some(read)), &f.root, &[]);
        assert!(!got.is_empty());
        let shown = format!("{got:?}");
        for secret in [TOKEN, TOKEN_ONE_BYTE_OFF, TOKEN_LONGER] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}

/// Spec §7.8: `CredentialShared` appears for a single-credential network enrollment and not for
/// a split or a local one; it is information, never a refusal.
#[test]
fn credential_shared_is_advice_for_single_credential_network_enrollments_only() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let read = put(&f.root, "read", TOKEN_ONE_BYTE_OFF, 0o600);
    let https = Endpoint::parse(HTTPS).unwrap();
    let broker_dir = f.root.parent().unwrap().join("git-push");

    let single = policy(HTTPS, &write, None);
    let got = check_enrollment(&single, &https, &[], &broker_dir, &f.root);
    assert!(
        got.contains(&EnrollmentProblem::CredentialShared),
        "{got:?}"
    );
    assert!(!EnrollmentProblem::CredentialShared.is_refusal());
    assert!(
        got.iter().all(|p| !p.is_refusal()),
        "the hint must not refuse an otherwise-valid enrollment: {got:?}"
    );

    let split = policy(HTTPS, &write, Some(&read));
    let got = check_enrollment(&split, &https, &[], &broker_dir, &f.root);
    assert!(
        !got.contains(&EnrollmentProblem::CredentialShared),
        "{got:?}"
    );

    let remote = f.root.parent().unwrap().join("remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    let local = Endpoint::parse(&format!("file://{}", remote.display())).unwrap();
    let mut no_cred = policy(local.as_str(), &write, None);
    no_cred.credential = None;
    let got = check_enrollment(&no_cred, &local, &[], &broker_dir, &f.root);
    assert!(
        !got.contains(&EnrollmentProblem::CredentialShared),
        "{got:?}"
    );

    // An https remote enrolled anonymously has no credential to share.
    let mut anon = policy(HTTPS, &write, None);
    anon.credential = None;
    let got = check_enrollment(&anon, &https, &[], &broker_dir, &f.root);
    assert!(
        !got.contains(&EnrollmentProblem::CredentialShared),
        "{got:?}"
    );
}

/// `CredentialShared` (no read credential) and `CredentialSameSecret` (a read credential that is
/// the same token) never appear together.
#[test]
fn shared_and_same_secret_are_mutually_exclusive() {
    let f = fx();
    let write = put(&f.root, "write", TOKEN, 0o600);
    let twin = put(&f.root, "twin", TOKEN, 0o600);
    let https = Endpoint::parse(HTTPS).unwrap();
    let broker_dir = f.root.parent().unwrap().join("git-push");
    for read in [None, Some(twin.as_path())] {
        let got = check_enrollment(
            &policy(HTTPS, &write, read),
            &https,
            &[],
            &broker_dir,
            &f.root,
        );
        let shared = got.contains(&EnrollmentProblem::CredentialShared);
        assert_ne!(shared, same_secret(&got), "{got:?}");
    }
}
