//! Where git actually looks for hooks in a repo (#1672).
//!
//! Hard-coding `<repo>/.git/hooks` breaks in three common setups: a repo with
//! `core.hooksPath` (husky, lefthook, a custom dir), a linked worktree, and a
//! submodule — in the last two `.git` is a file, not a directory. Git itself
//! knows the answer, so ask it.

use std::path::{Path, PathBuf};

/// The hooks directory for the repo at `project_path`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum HooksDir {
    /// A directory git reads and MUR may write its hook block into.
    Writable(PathBuf),
    /// `core.hooksPath` points inside the working tree (e.g. `.husky/`): those
    /// files are usually versioned, so MUR must not edit them behind the
    /// user's back.
    InWorkTree(PathBuf),
    /// No hooks directory to use: not a git repo, or the directory is missing.
    None,
}

/// Resolve the hooks directory git uses for `project_path`.
///
/// Falls back to the legacy `<repo>/.git/hooks` only when git cannot be asked
/// (binary missing, or a directory git does not recognise as a repo), so a
/// machine without git behaves exactly as before.
pub(super) fn resolve(project_path: &Path) -> HooksDir {
    let Some((hooks, toplevel)) = ask_git(project_path) else {
        let legacy = project_path.join(".git").join("hooks");
        return if legacy.is_dir() {
            HooksDir::Writable(legacy)
        } else {
            HooksDir::None
        };
    };
    let hooks_real = hooks.canonicalize().unwrap_or_else(|_| hooks.clone());
    let top_real = toplevel.canonicalize().unwrap_or(toplevel);
    // `.git/hooks` of the main checkout sits under the toplevel too, but `.git`
    // is git's own directory, not a tracked file.
    let under_git_dir = hooks_real
        .strip_prefix(&top_real)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|first| first.as_os_str() == ".git");
    if hooks_real.starts_with(&top_real) && !under_git_dir {
        return HooksDir::InWorkTree(hooks);
    }
    if hooks.is_dir() {
        HooksDir::Writable(hooks)
    } else {
        HooksDir::None
    }
}

/// `(hooks dir, worktree toplevel)` as git reports them, both absolute.
fn ask_git(project_path: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(project_path)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "hooks",
            "--show-toplevel",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let hooks = PathBuf::from(lines.next()?);
    let toplevel = PathBuf::from(lines.next()?);
    Some((hooks, toplevel))
}
