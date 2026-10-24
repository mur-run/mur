//! Where the broker learns what is enrolled, and whether a request still matches it.
//!
//! Nothing here caches. Every decision (a new request, an approval) asks the source again, so a
//! revoke or a policy edit takes effect at the next decision with no restart. A source that
//! cannot answer is an error, never "nothing enrolled": an unreadable file must not turn into
//! either a free pass or a silent void of pending work.

use crate::{
    action::{ActionDocument, RefUpdate},
    constants::ACTION_VERSION,
    credential::Credential,
    enrollment::Enrollments,
    error::BrokerError,
    policy::RemotePolicy,
    request_file::RequestFile,
};
use std::path::PathBuf;

/// The enrollment records, read fresh on every call.
pub trait EnrollmentSource: Send + Sync {
    /// `Ok(None)`: nothing is enrolled (the file does not exist). `Err`: it exists but cannot be
    /// trusted (unreadable or invalid); callers fail closed.
    fn current(&self) -> Result<Option<Enrollments>, BrokerError>;
}

/// `enrollments.yaml` on disk.
pub struct FileEnrollments {
    path: PathBuf,
}

impl FileEnrollments {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl EnrollmentSource for FileEnrollments {
    fn current(&self) -> Result<Option<Enrollments>, BrokerError> {
        Enrollments::load(&self.path).map_err(|_| BrokerError::EnrollmentUnavailable)
    }
}

/// Why a request or document does not line up with the current records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// No record binds this (agent, repo, remote).
    NotEnrolled,
    /// A record exists but something the document pinned has moved.
    Changed,
}

/// What execution needs from a binding that still holds. Owned, so it outlives the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub remote_url: String,
    pub policy: RemotePolicy,
    /// Where the secret lives and who it belongs to; never the secret. Safe under `Debug`.
    pub credential: Option<Credential>,
    /// Where the pre-approval (prefetch) secret lives; `None` means `credential` serves both
    /// phases. Copied from the enrollment record like `credential`, not from the policy.
    pub read_credential: Option<Credential>,
}

/// The document for a verified request, stamped with the *current* epoch and identity. The agent
/// supplies none of those; they come from the records.
pub fn build_action(e: &Enrollments, req: &RequestFile) -> Result<ActionDocument, Mismatch> {
    let b = e
        .binding(&req.agent, &req.repo_id, &req.remote_id)
        .ok_or(Mismatch::NotEnrolled)?;
    let p = b.enrollment.policy();
    Ok(ActionDocument {
        version: ACTION_VERSION.into(),
        agent_id: req.agent.clone(),
        task_id: req.task_id.clone(),
        enrollment_epoch: b.enrollment.epoch,
        request_id: req.request_id.clone(),
        repo_id: req.repo_id.clone(),
        repo_identity: b.repo.identity.clone(),
        object_format: b.repo.object_format,
        remote_id: req.remote_id.clone(),
        canonical_remote_endpoint: p.canonical_remote_endpoint.clone(),
        remote_policy_digest: p.remote_digest(),
        ref_policy_digest: p.digest(),
        updates: vec![RefUpdate {
            r#ref: req.r#ref.clone(),
            old_sha: req.old_sha.clone(),
            new_sha: req.new_sha.clone(),
        }],
        force: false,
        delete: false,
    })
}

/// Does `doc` still describe the current records? Four things are pinned and all must hold: the
/// record exists, its epoch, the repo's identity, and the policy digests. Anything else that the
/// document repeats (endpoint, object format) must agree too.
pub fn bind(e: &Enrollments, doc: &ActionDocument) -> Result<Bound, Mismatch> {
    let b = e
        .binding(&doc.agent_id, &doc.repo_id, &doc.remote_id)
        .ok_or(Mismatch::NotEnrolled)?;
    let p = b.enrollment.policy();
    let holds = doc.enrollment_epoch == b.enrollment.epoch
        && doc.repo_identity == b.repo.identity
        && doc.object_format == b.repo.object_format
        && doc.ref_policy_digest == p.digest()
        && doc.remote_policy_digest == p.remote_digest()
        && doc.canonical_remote_endpoint == p.canonical_remote_endpoint
        && doc
            .updates
            .first()
            .is_some_and(|u| u.r#ref.starts_with(&p.ref_prefix));
    if !holds {
        return Err(Mismatch::Changed);
    }
    Ok(Bound {
        remote_url: b.enrollment.endpoint.as_str().to_string(),
        policy: p,
        credential: b.enrollment.credential.clone(),
        read_credential: b.enrollment.read_credential.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{SecretPath, Username};
    use crate::endpoint::Endpoint;
    use crate::enrollment::{EnrollRequest, NewRepo};
    use crate::oid::ObjectFormat;

    const SHA: &str = "1111111111111111111111111111111111111111";

    fn enroll(c: Option<Credential>) -> Enrollments {
        let mut e = Enrollments::empty(100);
        e.enroll(EnrollRequest {
            agent: "alice".into(),
            repo_id: "mur".into(),
            remote_id: "origin".into(),
            endpoint: Endpoint::parse("https://git.example.com/o/r.git").unwrap(),
            ref_prefix: "refs/heads/agent/alice/".into(),
            creation_base_refs: vec![],
            credential: c,
            read_credential: None,
            new_repo: Some(NewRepo {
                path: "/work/mur".into(),
                object_format: ObjectFormat::Sha1,
            }),
        })
        .unwrap();
        e
    }
    fn request() -> RequestFile {
        RequestFile {
            agent: "alice".into(),
            task_id: "t".into(),
            request_id: "r1".into(),
            repo_id: "mur".into(),
            remote_id: "origin".into(),
            r#ref: "refs/heads/agent/alice/x".into(),
            old_sha: SHA.into(),
            new_sha: SHA.into(),
            pack_sha256: "0".repeat(64),
            requested_at: chrono::Utc::now(),
            key_version: 1,
            sig: String::new(),
        }
    }
    fn cred() -> Credential {
        Credential {
            secret_path: SecretPath::parse("/h/.mur/secrets/gh").unwrap(),
            username: Username::parse("x-access-token").unwrap(),
        }
    }
    fn read_cred() -> Credential {
        Credential {
            secret_path: SecretPath::parse("/h/.mur/secrets/gh-read").unwrap(),
            username: Username::parse("reader").unwrap(),
        }
    }

    #[test]
    fn bind_carries_the_credential_record_from_the_enrollment() {
        for c in [Some(cred()), None] {
            let e = enroll(c.clone());
            let doc = build_action(&e, &request()).unwrap();
            let bound = bind(&e, &doc).unwrap();
            assert_eq!(bound.credential, c);
            assert_eq!(bound.remote_url, "https://git.example.com/o/r.git");
        }
    }

    #[test]
    fn bind_carries_the_read_credential_record_from_the_enrollment() {
        let mut e = Enrollments::empty(100);
        e.enroll(EnrollRequest {
            agent: "alice".into(),
            repo_id: "mur".into(),
            remote_id: "origin".into(),
            endpoint: Endpoint::parse("https://git.example.com/o/r.git").unwrap(),
            ref_prefix: "refs/heads/agent/alice/".into(),
            creation_base_refs: vec![],
            credential: Some(cred()),
            read_credential: Some(read_cred()),
            new_repo: Some(NewRepo {
                path: "/work/mur".into(),
                object_format: ObjectFormat::Sha1,
            }),
        })
        .unwrap();
        let bound = bind(&e, &build_action(&e, &request()).unwrap()).unwrap();
        assert_eq!(bound.credential, Some(cred()));
        assert_eq!(bound.read_credential, Some(read_cred()));
    }

    /// Spec §7.10: enroll with both, save, reload into a fresh `Enrollments`; the reloaded
    /// policy and `Bound` carry the same credentials and the policy digest is unchanged.
    #[test]
    fn a_split_enrollment_survives_save_and_reload_with_the_same_policy_and_binding() {
        let mut e = Enrollments::empty(100);
        e.enroll(EnrollRequest {
            agent: "alice".into(),
            repo_id: "mur".into(),
            remote_id: "origin".into(),
            endpoint: Endpoint::parse("https://git.example.com/o/r.git").unwrap(),
            ref_prefix: "refs/heads/agent/alice/".into(),
            creation_base_refs: vec![],
            credential: Some(cred()),
            read_credential: Some(read_cred()),
            new_repo: Some(NewRepo {
                path: "/work/mur".into(),
                object_format: ObjectFormat::Sha1,
            }),
        })
        .unwrap();
        let before = e.find("alice", "mur", "origin").unwrap().policy();
        let dir = tempfile::tempdir().unwrap();
        e.save(dir.path()).unwrap();
        let back = Enrollments::load(&mur_common::git_push::enrollments_path(dir.path()))
            .unwrap()
            .unwrap();
        let after = back.find("alice", "mur", "origin").unwrap().policy();
        assert_eq!(after.credential, Some(cred()));
        assert_eq!(after.read_credential, Some(read_cred()));
        assert_eq!(after.digest(), before.digest());
        assert_eq!(after.remote_digest(), before.remote_digest());
        let doc = build_action(&back, &request()).unwrap();
        let bound = bind(&back, &doc).unwrap();
        assert_eq!(bound.credential, after.credential);
        assert_eq!(bound.read_credential, after.read_credential);
        assert_eq!(bound.policy, after);
    }

    #[test]
    fn bound_debug_has_the_remote_without_userinfo_and_the_secret_location_only() {
        let e = enroll(Some(cred()));
        let bound = bind(&e, &build_action(&e, &request()).unwrap()).unwrap();
        let d = format!("{bound:?}");
        assert!(d.contains("/h/.mur/secrets/gh"), "{d}");
        assert!(!d.contains('@'), "no userinfo in a loggable value: {d}");
    }
}
