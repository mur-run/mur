//! The lease push and the mapping of its result (design §5, F15/F21).
//!
//! One `git push --porcelain --force-with-lease=<ref>:<old>` per approved request. A creation
//! uses the empty lease (`<ref>:`), so a ref that appeared meanwhile fails as a stale lease.
//! There is no retry anywhere: a push whose outcome we cannot read is `OutcomeUnknown`.

use crate::{
    action::ActionDocument,
    error::BrokerError,
    git::{GitError, run_with_timeout},
    policy::BrokerLimits,
    repo::PrivateRepo,
};
use std::{process::Command, time::Duration};

/// Adds whatever the transport needs (credentials, ssh command) to the scrubbed push command.
pub trait RemoteAuth: Send + Sync {
    fn apply(&self, cmd: &mut Command);
}

/// No credentials: local and file remotes.
pub struct NoAuth;
impl RemoteAuth for NoAuth {
    fn apply(&self, _cmd: &mut Command) {}
}

fn unknown(why: &str) -> BrokerError {
    BrokerError::OutcomeUnknown(why.into())
}

/// Push the single update. Before any process is spawned the private repo is re-checked: the
/// new tip must still be a commit and the control files must still match what was frozen at
/// submit time; either failure is `OutcomeUnknown` and nothing is sent.
pub fn push(
    repo: &PrivateRepo,
    action: &ActionDocument,
    remote_url: &str,
    auth: &dyn RemoteAuth,
    limits: &BrokerLimits,
    frozen_digest: &str,
) -> Result<(), BrokerError> {
    let u = &action.updates[0];
    let check = format!("{}^{{commit}}", u.new_sha);
    let verified = repo
        .runner()
        .run(
            &["rev-parse", "--verify", &check],
            Duration::from_secs(limits.git_timeout_secs),
        )
        .map_err(|_| unknown("new_sha check failed"))?;
    if verified.code != 0 {
        return Err(unknown("new_sha is not a commit"));
    }
    match repo.control_digest() {
        Ok(d) if d == frozen_digest => {}
        _ => return Err(unknown("control files changed")),
    }
    let lease = if action.is_creation() {
        format!("--force-with-lease={}:", u.r#ref)
    } else {
        format!("--force-with-lease={}:{}", u.r#ref, u.old_sha)
    };
    let refspec = format!("{}:{}", u.new_sha, u.r#ref);
    let mut cmd = repo
        .runner()
        .command(&["push", "--porcelain", &lease, remote_url, &refspec]);
    auth.apply(&mut cmd);
    match run_with_timeout(cmd, Duration::from_secs(limits.push_timeout_secs)) {
        Ok(out) => parse_push_outcome(out.code, &String::from_utf8_lossy(&out.stdout)),
        Err(GitError::Timeout | GitError::Signal | GitError::Tripped) => {
            Err(unknown("git terminated during push"))
        }
        Err(GitError::Spawn(_)) => Err(unknown("git did not start")),
    }
}

/// Read `--porcelain` stdout only. The reason is always a fixed code, never remote text: a
/// server hook message can carry attacker-chosen bytes.
pub fn parse_push_outcome(code: i32, porcelain: &str) -> Result<(), BrokerError> {
    let rejected = porcelain
        .lines()
        .find(|l| l.starts_with('!') && l.contains('\t') && l.contains(":refs/"));
    match rejected {
        Some(l) if l.contains("(stale info)") => Err(BrokerError::StaleOldSha),
        Some(_) => Err(BrokerError::Rejected("remote_rejected".into())),
        None if code == 0 => Ok(()),
        None => Err(BrokerError::Rejected(format!("git_exit_{code}"))),
    }
}
