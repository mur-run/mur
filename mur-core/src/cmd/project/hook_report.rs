//! Human wording for [`HookHealth`] (#1672 point 3), shared by `mur doctor`,
//! `mur project status` and `mur project index` so the three cannot disagree.

use std::path::Path;

use crate::codebase::{HookHealth, MANUAL_HOOK_CMD, hook_health};

/// `Ok(summary)` when auto-index will run on commit (or there is no repo to
/// check); `Err(problem)` with the fix spelled out when it will not.
pub(crate) fn describe(health: &HookHealth) -> std::result::Result<String, String> {
    match health {
        HookHealth::Active => Ok("active (reindexes on commit)".into()),
        HookHealth::NotARepo => Ok("not a git repo (no hook needed)".into()),
        HookHealth::NotInstalled {
            hooks_dir,
            in_work_tree: true,
        } => Err(format!(
            "not active: core.hooksPath is {} (versioned, MUR does not edit it). Add to its post-commit: {MANUAL_HOOK_CMD}",
            hooks_dir.display()
        )),
        HookHealth::NotInstalled { .. } => {
            Err("not installed. Run `mur project index` to install it.".into())
        }
        HookHealth::Stranded { hook, hooks_dir } => Err(format!(
            "not active: MUR's block is in {}, but git runs hooks from {}. Run `mur project index`, or add to that dir's post-commit: {MANUAL_HOOK_CMD}",
            hook.display(),
            hooks_dir.display()
        )),
        HookHealth::NotExecutable { hook } => Err(format!(
            "not active: {} is not executable, so git skips it. Run `chmod +x {}`.",
            hook.display(),
            hook.display()
        )),
    }
}

/// One-line verdict for `project_path`, or `None` when there is no repo.
pub(crate) fn check(project_path: &Path) -> Option<std::result::Result<String, String>> {
    let health = hook_health(project_path);
    (health != HookHealth::NotARepo).then(|| describe(&health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn every_inactive_state_names_a_fix() {
        let dir = PathBuf::from("/r/.husky");
        let hook = PathBuf::from("/r/.git/hooks/post-commit");
        for h in [
            HookHealth::NotInstalled {
                hooks_dir: dir.clone(),
                in_work_tree: true,
            },
            HookHealth::NotInstalled {
                hooks_dir: dir.clone(),
                in_work_tree: false,
            },
            HookHealth::Stranded {
                hook: hook.clone(),
                hooks_dir: dir.clone(),
            },
            HookHealth::NotExecutable { hook: hook.clone() },
        ] {
            let msg = describe(&h).expect_err("inactive must be a problem");
            assert!(
                msg.contains("mur project index") || msg.contains("chmod"),
                "{h:?} gives no fix: {msg}"
            );
        }
        assert!(describe(&HookHealth::Active).is_ok());
    }
}
