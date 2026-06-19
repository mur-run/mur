//! Canonical project identity for skill `scope: Project` matching.
//!
//! A project == its git repo root. Harvest stamps a learned skill with the
//! repo root it was learned in; injection computes the repo root of the current
//! working dir; the two match iff you're in the same repo. Using the repo root
//! (not the cwd) means a skill learned in a subdir of repo X still applies
//! anywhere in X.

use std::path::{Path, PathBuf};

/// Walk up from `start` to the directory containing a `.git` entry (the repo
/// root). `None` if `start` isn't inside a git repo.
///
/// Note: a `git worktree` has its own `.git` *file*, so it resolves to the
/// worktree's own root — a worktree and its main checkout get distinct project
/// ids (project-scoped skills don't cross between them). Acceptable for now.
pub fn repo_root_of(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Canonical project id (repo-root path string) for scope matching, or `None`
/// if not in a repo. Canonicalized so two entry points into the same repo
/// (subdir, symlink, relative path) produce the same id.
pub fn project_id(start: &Path) -> Option<String> {
    let root = repo_root_of(start)?;
    let canon = std::fs::canonicalize(&root).unwrap_or(root);
    // Non-UTF-8 repo path → None (treated as "not in a project" → global) so a
    // lossy id can never silently mismatch between stamp and inject time.
    canon.to_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn repo_root_found_from_subdir_and_none_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("myrepo");
        let sub = root.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();

        // from a nested subdir we find the repo root
        assert_eq!(repo_root_of(&sub).as_deref(), Some(root.as_path()));
        // project_id is the canonicalized repo root, stable across entry points
        assert_eq!(project_id(&sub), project_id(&root));
        // a dir with no .git ancestor → None
        let bare = tmp.path().join("not-a-repo");
        fs::create_dir_all(&bare).unwrap();
        assert!(repo_root_of(&bare).is_none());
        assert!(project_id(&bare).is_none());
    }
}
