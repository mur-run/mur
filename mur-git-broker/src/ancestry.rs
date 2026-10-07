//! Fast-forward judgement (F12). Ancestry is proved by git inside the scrubbed private repo, and
//! only exit codes 0 and 1 mean anything: every other outcome is "cannot prove", never "not a
//! fast-forward", so a broken environment cannot masquerade as a verdict.
use crate::{
    action::ActionDocument,
    error::BrokerError,
    git::{GitError, GitOutput},
    policy::BrokerLimits,
    repo::PrivateRepo,
};
use std::time::Duration;

/// Pure exit-code mapping for `git merge-base --is-ancestor`.
pub fn classify(r: Result<GitOutput, GitError>) -> Result<(), BrokerError> {
    match r {
        Ok(o) if o.code == 0 => Ok(()),
        Ok(o) if o.code == 1 => Err(BrokerError::NotFastForward),
        Ok(o) => Err(BrokerError::AncestryUnprovable(format!("exit {}", o.code))),
        Err(e) => Err(BrokerError::AncestryUnprovable(format!("{e:?}"))),
    }
}

pub fn judge(
    repo: &PrivateRepo,
    action: &ActionDocument,
    limits: &BrokerLimits,
) -> Result<(), BrokerError> {
    // Creation has no ancestry to prove: the push lease is "ref must not exist" (§5 item 8).
    if action.is_creation() {
        return Ok(());
    }
    // Grafts, shallow files, alternates and commit-graphs can all change what git believes.
    repo.inspect_forbidden()?;
    let u = &action.updates[0];
    classify(repo.runner().run(
        &["merge-base", "--is-ancestor", &u.old_sha, &u.new_sha],
        Duration::from_secs(limits.git_timeout_secs),
    ))
}
