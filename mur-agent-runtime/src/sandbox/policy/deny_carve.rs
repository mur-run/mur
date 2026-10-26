//! `fs_deny` on a backend that cannot carve a deny out of a grant (#2160).
//!
//! Landlock is a pure allow-list: once a path is granted, nothing beneath it
//! can be closed again. A profile like `write: [~/work]` + `deny:
//! [~/work/secrets]` therefore cannot be installed as written. macOS reaches
//! the documented posture with a `deny` clause emitted after the allows
//! (last-match-wins); Linux has no such clause, so it goes the other way —
//! fail-closed, the same rule `LaunchChain::partition_grants` applies to the
//! launch chain:
//!
//! * A **user-declared** grant (and anything derived from one, such as a git
//!   worktree) that overlaps a deny in either direction is dropped whole and
//!   recorded, so `mur agent perm list-paths` shows it as `✗ dropped`.
//! * A **runtime-owned** grant (the agent's own home, `channels/`, system read
//!   paths, …) cannot be dropped without breaking the runtime. A deny under
//!   one of those is recorded as not enforced by the kernel instead of being
//!   silently ignored. The file tools still refuse it (`tools::fs_policy`);
//!   shell commands do not.
//!
//! The partition helpers are platform-neutral so they are tested everywhere;
//! only the call sites are Linux-gated.

use std::path::{Path, PathBuf};

use super::{SELF_PROTECTED_AGENT_FILES, SandboxPolicy};

/// `true` when either path contains the other (component-wise).
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Split user-declared grants into those that overlap no deny and those that
/// must be dropped whole. Grants inside the agent's own home are exempt: the
/// runtime force-grants that directory anyway, so dropping the user's copy
/// would change nothing but the report. Each dropped grant comes back with the
/// first deny it collided with, for the recorded reason.
pub(crate) fn partition_against_denies(
    grants: Vec<PathBuf>,
    denies: &[PathBuf],
    self_home: &Path,
) -> (Vec<PathBuf>, Vec<(PathBuf, PathBuf)>) {
    let mut kept = Vec::with_capacity(grants.len());
    let mut dropped = Vec::new();
    for g in grants {
        if g.starts_with(self_home) {
            kept.push(g);
            continue;
        }
        match denies.iter().find(|d| overlaps(&g, d)) {
            Some(d) => dropped.push((g, d.clone())),
            None => kept.push(g),
        }
    }
    (kept, dropped)
}

/// Record a dropped grant: warn once and add the `DroppedGrant` row the lock
/// file carries to `perm list-paths`.
pub(crate) fn record_deny_drops(
    dropped: &[(PathBuf, PathBuf)],
    verb: &str,
    out: &mut Vec<mur_common::agent::DroppedGrant>,
) {
    for (grant, deny) in dropped {
        tracing::warn!(
            grant = %grant.display(),
            deny = %deny.display(),
            verb,
            "filesystem grant overlaps an fs_deny entry; Landlock cannot carve a \
             deny out of a grant, so the whole grant is dropped (#2160)"
        );
        out.push(mur_common::agent::DroppedGrant {
            path: grant.display().to_string(),
            verb: verb.into(),
            reason: format!(
                "overlaps fs_deny {} and cannot be carved (Landlock)",
                deny.display()
            ),
        });
    }
}

impl SandboxPolicy {
    /// User `fs_deny` entries the kernel will not enforce because a grant that
    /// survived the build still overlaps them, paired with that grant. On
    /// Linux every grant left at this point is runtime-owned (user grants that
    /// overlapped were dropped by [`partition_against_denies`]), so this is
    /// exactly the set of denies only the tool gate stands behind.
    ///
    /// The runtime's own `SELF_PROTECTED_AGENT_FILES` denies are excluded:
    /// they are not user entries, and their Linux posture is documented on
    /// `LaunchChain` (macOS tier 3 only, plus the tool gate).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn unenforceable_denies(&self) -> Vec<(PathBuf, PathBuf)> {
        let self_home = self.launch_chain.agent_self_home();
        let self_protected: Vec<PathBuf> = SELF_PROTECTED_AGENT_FILES
            .iter()
            .map(|f| self_home.join(f))
            .collect();
        self.fs_deny
            .iter()
            .filter(|d| !self_protected.contains(d))
            .filter_map(|d| {
                self.fs_read
                    .iter()
                    .chain(&self.fs_write)
                    .chain(&self.fs_exec)
                    .find(|g| overlaps(g, d))
                    .map(|g| (d.clone(), g.clone()))
            })
            .collect()
    }

    /// Record every [`Self::unenforceable_denies`] entry as a `deny` row in
    /// `dropped`, so the report says the deny did not reach the kernel rather
    /// than showing it as if it held. Called on the finished policy (after
    /// `build_policy` widened it), Linux only.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn record_unenforceable_denies(&mut self) {
        for (deny, grant) in self.unenforceable_denies() {
            tracing::warn!(
                deny = %deny.display(),
                grant = %grant.display(),
                "fs_deny entry lies inside a runtime-owned grant; Landlock cannot \
                 carve it, so the kernel does NOT enforce it (file tools still \
                 refuse it, shell commands do not) (#2160)"
            );
            self.dropped.push(mur_common::agent::DroppedGrant {
                path: deny.display().to_string(),
                verb: "deny".into(),
                reason: format!(
                    "not enforced by the kernel: inside runtime-owned grant {}, which \
                     Landlock cannot carve; file tools still refuse it, shell commands do not",
                    grant.display()
                ),
            });
        }
    }
}

#[cfg(test)]
#[path = "deny_carve_tests.rs"]
mod tests;
