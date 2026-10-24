//! Enrollment doctor: data in, problems out. Shared by `mur git-push enroll` (which refuses on
//! any refusal) and `mur git-push doctor`, and by the daemon's start-up check, so the three
//! cannot disagree about what a valid enrollment is.
//!
//! The inputs about agents are collected by the caller (`mur-core` can compute sandbox grants;
//! this crate sits below it and must not).

use crate::constants::{ALLOWED_REF_PREFIX, ENDPOINT_HTTPS_SCHEME};
use crate::credential_source::{CredentialSource, FileCredentials};
use crate::enrollment::Endpoint;
use crate::policy::RemotePolicy;
use secrecy::{ExposeSecret, SecretString};
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;

/// What the doctor needs to know about one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacts {
    pub name: String,
    pub home: PathBuf,
    /// Every path the agent's sandbox lets it write (entitlements plus the runtime's own).
    pub write_paths: Vec<PathBuf>,
    /// `entitlements.fail_closed_on_sandbox_error`. `false` lets the agent run unsandboxed if
    /// the sandbox fails to apply.
    pub fail_closed: bool,
    /// What the agent's `running.lock` says the sandbox did, when it is running.
    pub sandbox_enforcing: Option<bool>,
}

impl AgentFacts {
    /// Could this agent be (or currently is) running without a kernel sandbox?
    pub fn may_run_unsandboxed(&self) -> bool {
        !self.fail_closed || self.sandbox_enforcing == Some(false)
    }
}

/// A reason the doctor objects to an enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentProblem {
    /// A `creation_base_refs` entry that is not a full `refs/heads/<name>`.
    BaseRefNotFullHead(String),
    /// `ref_prefix` reaches outside the agent namespace.
    RefPrefixTooWide(String),
    /// An agent write grant overlaps the broker-owned directory. Report only: the sandbox
    /// itself closes this (Linux drops the grant, macOS denies it).
    GrantCoversBrokerDir(PathBuf),
    /// The stored endpoint is not the real path (a symlink or an alias in it).
    EndpointNotCanonical(PathBuf),
    /// The endpoint does not exist, so its real path cannot be checked.
    EndpointMissing(PathBuf),
    /// An agent can write to (or inside, or above) the remote. A hook in the remote runs with
    /// the broker's privileges, so no agent may reach it.
    EndpointWritableByAgent { agent: String, path: PathBuf },
    /// An agent may run without its sandbox, and could then rewrite the enrollment file.
    AgentMayRunUnsandboxed(String),
    /// The endpoint is a network remote: the canonical / missing / agent-writable path checks
    /// do not apply to it. Information, never a refusal.
    EndpointPathChecksNotApplicable(String),
    /// The credential's secret cannot be used as the broker would use it. The reason is the same
    /// fixed token the broker refuses with (no path, no content).
    CredentialUnusable(&'static str),
    /// An agent can write to (or above, or inside) the secrets directory, and could then swap
    /// the credential for one of its own.
    SecretsWritableByAgent { agent: String, path: PathBuf },
    /// The read and write secrets hold the same token (same file, a link to it, or equal
    /// contents), so the read credential could not keep the push token unopened before approval.
    /// Carries no payload, so it cannot carry a secret.
    CredentialSameSecret,
    /// A network enrollment has a credential and no read credential: prefetch opens the push
    /// secret before approval. Advice, never a refusal.
    CredentialShared,
}

impl EnrollmentProblem {
    /// Whether `enroll` must refuse. Everything else is information.
    pub fn is_refusal(&self) -> bool {
        !matches!(
            self,
            Self::GrantCoversBrokerDir(_)
                | Self::EndpointPathChecksNotApplicable(_)
                | Self::CredentialShared
        )
    }
}

/// Policy fields only: the same rules the broker applies, so they cannot drift.
pub fn policy_problems(p: &RemotePolicy) -> Vec<EnrollmentProblem> {
    let mut out = Vec::new();
    let prefix_only = RemotePolicy {
        creation_base_refs: Vec::new(),
        ..p.clone()
    };
    if prefix_only.validate_enrollment().is_err() {
        out.push(EnrollmentProblem::RefPrefixTooWide(p.ref_prefix.clone()));
    }
    for r in &p.creation_base_refs {
        let one = RemotePolicy {
            creation_base_refs: vec![r.clone()],
            ref_prefix: ALLOWED_REF_PREFIX.to_string(),
            ..p.clone()
        };
        if one.validate_enrollment().is_err() {
            out.push(EnrollmentProblem::BaseRefNotFullHead(r.clone()));
        }
    }
    out
}

/// Grants that overlap the broker directory, in either direction.
pub fn grant_problems(grants: &[PathBuf], broker_dir: &Path) -> Vec<EnrollmentProblem> {
    grants
        .iter()
        .filter(|g| broker_dir.starts_with(g) || g.starts_with(broker_dir))
        .map(|g| EnrollmentProblem::GrantCoversBrokerDir(g.clone()))
        .collect()
}

/// The remote is a real, existing path no agent can write to, and no agent may skip its sandbox.
///
/// The path checks apply to `file://` remotes only. A network remote is not on this machine's
/// filesystem, so they are not "passing", they are not applicable, and that is reported as
/// [`EnrollmentProblem::EndpointPathChecksNotApplicable`] instead of silence.
pub fn endpoint_problems(endpoint: &Endpoint, agents: &[AgentFacts]) -> Vec<EnrollmentProblem> {
    let mut out = Vec::new();
    match endpoint.path() {
        Some(stored) => {
            let real = match std::fs::canonicalize(stored) {
                Ok(r) => r,
                Err(_) => {
                    out.push(EnrollmentProblem::EndpointMissing(stored.to_path_buf()));
                    return out;
                }
            };
            if real != stored {
                out.push(EnrollmentProblem::EndpointNotCanonical(
                    stored.to_path_buf(),
                ));
            }
            for a in agents {
                let reach = std::iter::once(&a.home)
                    .chain(&a.write_paths)
                    .find(|w| overlaps(&real, w));
                if let Some(w) = reach {
                    out.push(EnrollmentProblem::EndpointWritableByAgent {
                        agent: a.name.clone(),
                        path: w.clone(),
                    });
                }
            }
        }
        None => out.push(EnrollmentProblem::EndpointPathChecksNotApplicable(
            endpoint.as_str().to_string(),
        )),
    }
    for a in agents {
        if a.may_run_unsandboxed() {
            out.push(EnrollmentProblem::AgentMayRunUnsandboxed(a.name.clone()));
        }
    }
    out
}

/// Either contains the other, comparing real paths where they exist (a grant given as a
/// symlink must not slip past).
fn overlaps(endpoint_real: &Path, grant: &Path) -> bool {
    let g = std::fs::canonicalize(grant).unwrap_or_else(|_| grant.to_path_buf());
    endpoint_real.starts_with(&g) || g.starts_with(endpoint_real)
}

/// The credential an enrollment carries, if any: the file must open the way the broker opens it
/// (same checks, via [`FileCredentials`]), and no agent may be able to write the secrets
/// directory or anything above it.
///
/// Nothing is reported for an enrollment without a credential: there is nothing to check.
pub fn credential_problems(
    policy: &RemotePolicy,
    secrets_root: &Path,
    agents: &[AgentFacts],
) -> Vec<EnrollmentProblem> {
    let Some(cred) = &policy.credential else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let source = FileCredentials::new(secrets_root);
    // Each secret is opened on its own, so each failure is reported on its own. Values live only
    // until the comparison below and are never printed; only refusal reasons are kept.
    let write = source.open(&cred.secret_path);
    let read = policy
        .read_credential
        .as_ref()
        .map(|r| source.open(&r.secret_path));
    for opened in std::iter::once(&write).chain(read.as_ref()) {
        if let Err(crate::error::BrokerError::CredentialUnreadable(why)) = opened {
            out.push(EnrollmentProblem::CredentialUnusable(why));
        }
    }
    // Compared only when both opened: an unusable secret is never reported as the same secret.
    if let (Ok(w), Some(Ok(r))) = (&write, &read)
        && same_secret(w, r)
    {
        out.push(EnrollmentProblem::CredentialSameSecret);
    }
    let real = std::fs::canonicalize(secrets_root).unwrap_or_else(|_| secrets_root.to_path_buf());
    for a in agents {
        let reach = std::iter::once(&a.home)
            .chain(&a.write_paths)
            .find(|w| overlaps(&real, w));
        if let Some(w) = reach {
            out.push(EnrollmentProblem::SecretsWritableByAgent {
                agent: a.name.clone(),
                path: w.clone(),
            });
        }
    }
    out
}

/// Do two secrets hold the same value? Different lengths differ at once (a length is not secret);
/// equal lengths are compared in constant time.
fn same_secret(a: &SecretString, b: &SecretString) -> bool {
    let (a, b) = (a.expose_secret().as_bytes(), b.expose_secret().as_bytes());
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// A network enrollment whose one credential serves both phases. Nothing for a local endpoint
/// (it opens nothing before approval), an anonymous one, or a split enrollment.
pub fn shared_credential_note(policy: &RemotePolicy) -> Vec<EnrollmentProblem> {
    let network = policy
        .canonical_remote_endpoint
        .starts_with(ENDPOINT_HTTPS_SCHEME);
    if network && policy.credential.is_some() && policy.read_credential.is_none() {
        vec![EnrollmentProblem::CredentialShared]
    } else {
        Vec::new()
    }
}

/// Everything the doctor checks for one enrollment.
pub fn check_enrollment(
    policy: &RemotePolicy,
    endpoint: &Endpoint,
    agents: &[AgentFacts],
    broker_dir: &Path,
    secrets_root: &Path,
) -> Vec<EnrollmentProblem> {
    let grants: Vec<PathBuf> = agents.iter().flat_map(|a| a.write_paths.clone()).collect();
    let mut out = policy_problems(policy);
    out.extend(endpoint_problems(endpoint, agents));
    out.extend(grant_problems(&grants, broker_dir));
    out.extend(credential_problems(policy, secrets_root, agents));
    out.extend(shared_credential_note(policy));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, home: &Path, writes: &[&Path]) -> AgentFacts {
        AgentFacts {
            name: name.into(),
            home: home.into(),
            write_paths: writes.iter().map(|p| p.to_path_buf()).collect(),
            fail_closed: true,
            sandbox_enforcing: Some(true),
        }
    }
    fn ep(p: &Path) -> Endpoint {
        Endpoint::parse(&format!("file://{}", p.display())).unwrap()
    }

    struct Fx {
        _t: tempfile::TempDir,
        root: PathBuf,
    }
    fn fx() -> Fx {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        Fx { _t: t, root }
    }

    #[test]
    fn a_remote_outside_every_agent_is_clean() {
        let f = fx();
        let remote = f.root.join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        let home = f.root.join("agents/a");
        assert_eq!(
            endpoint_problems(&ep(&remote), &[agent("a", &home, &[&home])]),
            vec![]
        );
    }

    #[test]
    fn overlap_counts_in_both_directions() {
        let f = fx();
        let remote = f.root.join("work/remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        for grant in [f.root.join("work"), remote.clone(), remote.join("hooks")] {
            let a = agent("a", &f.root.join("h"), &[&grant]);
            let got = endpoint_problems(&ep(&remote), &[a]);
            assert!(
                matches!(
                    &got[..],
                    [EnrollmentProblem::EndpointWritableByAgent { .. }]
                ),
                "{grant:?}: {got:?}"
            );
        }
    }

    #[test]
    fn a_symlinked_grant_does_not_hide_an_overlap() {
        let f = fx();
        let remote = f.root.join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        let link = f.root.join("alias");
        std::os::unix::fs::symlink(&remote, &link).unwrap();
        let got = endpoint_problems(&ep(&remote), &[agent("a", &f.root.join("h"), &[&link])]);
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn a_network_endpoint_reports_path_checks_as_not_applicable_not_as_clean() {
        let https = Endpoint::parse("https://git.example.com/o/r.git").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let a = agent("a", tmp.path(), &[tmp.path()]);
        let got = endpoint_problems(&https, &[a]);
        assert_eq!(
            got,
            vec![EnrollmentProblem::EndpointPathChecksNotApplicable(
                "https://git.example.com/o/r.git".into()
            )]
        );
        assert!(got.iter().all(|p| !p.is_refusal()));
    }

    #[test]
    fn a_network_endpoint_still_flags_an_unsandboxed_agent() {
        let https = Endpoint::parse("https://git.example.com/o/r.git").unwrap();
        let loose = AgentFacts {
            name: "loose".into(),
            home: "/h".into(),
            write_paths: vec![],
            fail_closed: false,
            sandbox_enforcing: None,
        };
        let got = endpoint_problems(&https, &[loose]);
        assert!(got.contains(&EnrollmentProblem::AgentMayRunUnsandboxed("loose".into())));
        assert!(got.iter().any(EnrollmentProblem::is_refusal));
    }

    #[test]
    fn a_symlinked_or_missing_endpoint_is_refused() {
        let f = fx();
        let remote = f.root.join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        let link = f.root.join("link.git");
        std::os::unix::fs::symlink(&remote, &link).unwrap();
        assert_eq!(
            endpoint_problems(&ep(&link), &[]),
            vec![EnrollmentProblem::EndpointNotCanonical(link)]
        );
        let gone = f.root.join("gone.git");
        assert_eq!(
            endpoint_problems(&ep(&gone), &[]),
            vec![EnrollmentProblem::EndpointMissing(gone)]
        );
    }

    #[test]
    fn an_agent_that_may_skip_its_sandbox_blocks_enrollment() {
        let f = fx();
        let remote = f.root.join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        let mut open = agent("dev", &f.root.join("h"), &[]);
        open.fail_closed = false;
        let mut dropped = agent("run", &f.root.join("h2"), &[]);
        dropped.sandbox_enforcing = Some(false);
        let got = endpoint_problems(&ep(&remote), &[open, dropped]);
        assert_eq!(
            got,
            vec![
                EnrollmentProblem::AgentMayRunUnsandboxed("dev".into()),
                EnrollmentProblem::AgentMayRunUnsandboxed("run".into()),
            ]
        );
        assert!(got.iter().all(EnrollmentProblem::is_refusal));
    }

    #[test]
    fn a_grant_over_the_broker_dir_is_reported_not_refused() {
        let g = grant_problems(&[PathBuf::from("/m")], Path::new("/m/git-push"));
        assert_eq!(
            g,
            vec![EnrollmentProblem::GrantCoversBrokerDir("/m".into())]
        );
        assert!(!g[0].is_refusal());
        assert!(grant_problems(&[PathBuf::from("/x")], Path::new("/m/git-push")).is_empty());
    }

    #[test]
    fn policy_problems_name_each_bad_field() {
        let p = RemotePolicy {
            remote_id: "o".into(),
            canonical_remote_endpoint: "file:///r".into(),
            creation_base_refs: vec!["main".into(), "refs/heads/ok".into()],
            ref_prefix: "refs/heads/".into(),
            credential: None,
            read_credential: None,
        };
        assert_eq!(
            policy_problems(&p),
            vec![
                EnrollmentProblem::RefPrefixTooWide("refs/heads/".into()),
                EnrollmentProblem::BaseRefNotFullHead("main".into()),
            ]
        );
    }

    // --- credential checks -------------------------------------------------------------------

    use crate::credential::{Credential, SecretPath, Username};
    use std::os::unix::fs::PermissionsExt;

    fn with_cred(secret: &Path) -> RemotePolicy {
        RemotePolicy {
            remote_id: "r".into(),
            canonical_remote_endpoint: "https://h/r.git".into(),
            creation_base_refs: vec!["refs/heads/main".into()],
            ref_prefix: ALLOWED_REF_PREFIX.into(),
            credential: Some(Credential {
                secret_path: SecretPath::parse(secret.to_str().unwrap()).unwrap(),
                username: Username::parse("bot").unwrap(),
            }),
            read_credential: None,
        }
    }
    fn secret(root: &Path, mode: u32) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let f = root.join("token");
        std::fs::write(&f, b"s3cret\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        f
    }

    #[test]
    fn a_usable_credential_outside_every_agent_is_clean() {
        let f = fx();
        let root = f.root.join("secrets");
        let p = with_cred(&secret(&root, 0o600));
        let home = f.root.join("agents/a");
        assert_eq!(
            credential_problems(&p, &root, &[agent("a", &home, &[&home])]),
            vec![]
        );
    }
    #[test]
    fn an_enrollment_without_a_credential_has_nothing_to_check() {
        let f = fx();
        // A root that does not exist and an agent that can write everywhere: still nothing,
        // because no credential means no secret to protect.
        let mut p = with_cred(Path::new("/x"));
        p.credential = None;
        let home = f.root.join("agents/a");
        assert_eq!(
            credential_problems(&p, &f.root.join("nope"), &[agent("a", &home, &[&f.root])]),
            vec![]
        );
    }
    #[test]
    fn a_missing_secret_file_is_reported_with_the_broker_reason() {
        let f = fx();
        let root = f.root.join("secrets");
        std::fs::create_dir_all(&root).unwrap();
        let p = with_cred(&root.join("absent"));
        assert_eq!(
            credential_problems(&p, &root, &[]),
            vec![EnrollmentProblem::CredentialUnusable("not_found")]
        );
    }
    #[test]
    fn a_secret_file_with_the_wrong_mode_is_reported() {
        let f = fx();
        let root = f.root.join("secrets");
        let p = with_cred(&secret(&root, 0o644));
        assert_eq!(
            credential_problems(&p, &root, &[]),
            vec![EnrollmentProblem::CredentialUnusable("bad_mode")]
        );
    }
    #[test]
    fn a_secret_outside_the_secrets_root_is_reported() {
        let f = fx();
        let root = f.root.join("secrets");
        std::fs::create_dir_all(&root).unwrap();
        let elsewhere = secret(&f.root.join("elsewhere"), 0o600);
        assert_eq!(
            credential_problems(&with_cred(&elsewhere), &root, &[]),
            vec![EnrollmentProblem::CredentialUnusable("outside_secrets")]
        );
    }
    #[test]
    fn an_agent_that_can_write_the_secrets_directory_is_reported() {
        let f = fx();
        let root = f.root.join("secrets");
        let p = with_cred(&secret(&root, 0o600));
        let home = f.root.join("agents/a");
        let problems = credential_problems(&p, &root, &[agent("a", &home, &[&root])]);
        assert_eq!(
            problems,
            vec![EnrollmentProblem::SecretsWritableByAgent {
                agent: "a".into(),
                path: root.clone()
            }]
        );
    }
    #[test]
    fn an_agent_that_can_write_above_the_secrets_directory_is_reported() {
        let f = fx();
        let root = f.root.join("mur/secrets");
        let p = with_cred(&secret(&root, 0o600));
        let home = f.root.join("agents/a");
        let above = f.root.join("mur");
        let problems = credential_problems(&p, &root, &[agent("a", &home, &[&above])]);
        assert!(matches!(
            problems.as_slice(),
            [EnrollmentProblem::SecretsWritableByAgent { .. }]
        ));
    }
    #[test]
    fn both_credential_problems_are_refusals() {
        assert!(EnrollmentProblem::CredentialUnusable("bad_mode").is_refusal());
        assert!(
            EnrollmentProblem::SecretsWritableByAgent {
                agent: "a".into(),
                path: "/p".into()
            }
            .is_refusal()
        );
    }
}
