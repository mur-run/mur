//! Enrollment doctor: data in, problems out. Shared by `mur git-push enroll` (which refuses on
//! any refusal) and `mur git-push doctor`, and by the daemon's start-up check, so the three
//! cannot disagree about what a valid enrollment is.
//!
//! The inputs about agents are collected by the caller (`mur-core` can compute sandbox grants;
//! this crate sits below it and must not).

use crate::constants::ALLOWED_REF_PREFIX;
use crate::enrollment::Endpoint;
use crate::policy::RemotePolicy;
use std::path::{Path, PathBuf};

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
}

impl EnrollmentProblem {
    /// Whether `enroll` must refuse. Everything else is information.
    pub fn is_refusal(&self) -> bool {
        !matches!(self, Self::GrantCoversBrokerDir(_))
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
pub fn endpoint_problems(endpoint: &Endpoint, agents: &[AgentFacts]) -> Vec<EnrollmentProblem> {
    let mut out = Vec::new();
    let stored = endpoint.path();
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

/// Everything the doctor checks for one enrollment.
pub fn check_enrollment(
    policy: &RemotePolicy,
    endpoint: &Endpoint,
    agents: &[AgentFacts],
    broker_dir: &Path,
) -> Vec<EnrollmentProblem> {
    let grants: Vec<PathBuf> = agents.iter().flat_map(|a| a.write_paths.clone()).collect();
    let mut out = policy_problems(policy);
    out.extend(endpoint_problems(endpoint, agents));
    out.extend(grant_problems(&grants, broker_dir));
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
        };
        assert_eq!(
            policy_problems(&p),
            vec![
                EnrollmentProblem::RefPrefixTooWide("refs/heads/".into()),
                EnrollmentProblem::BaseRefNotFullHead("main".into()),
            ]
        );
    }
}
