//! Where the broker learns what is enrolled, and whether a request still matches it.
//!
//! Nothing here caches. Every decision (a new request, an approval) asks the source again, so a
//! revoke or a policy edit takes effect at the next decision with no restart. A source that
//! cannot answer is an error, never "nothing enrolled": an unreadable file must not turn into
//! either a free pass or a silent void of pending work.

use crate::{
    action::{ActionDocument, RefUpdate},
    constants::ACTION_VERSION,
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
    })
}
