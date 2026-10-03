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
    let Some(git) = ask_git(project_path) else {
        let legacy = project_path.join(".git").join("hooks");
        return if legacy.is_dir() {
            HooksDir::Writable(legacy)
        } else {
            HooksDir::None
        };
    };
    let hooks = git.hooks;
    if in_work_tree(&hooks, &git.toplevel) {
        return HooksDir::InWorkTree(hooks);
    }
    if hooks.is_dir() {
        HooksDir::Writable(hooks)
    } else {
        HooksDir::None
    }
}

/// Whether `hooks` is a tracked-tree directory (not git's own `.git/`).
fn in_work_tree(hooks: &Path, toplevel: &Path) -> bool {
    let hooks_real = hooks.canonicalize().unwrap_or_else(|_| hooks.to_path_buf());
    let top_real = toplevel
        .canonicalize()
        .unwrap_or_else(|_| toplevel.to_path_buf());
    // `.git/hooks` of the main checkout sits under the toplevel too, but `.git`
    // is git's own directory, not a tracked file.
    let under_git_dir = hooks_real
        .strip_prefix(&top_real)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|first| first.as_os_str() == ".git");
    hooks_real.starts_with(&top_real) && !under_git_dir
}

/// What git reports about a repo's hook locations, all absolute.
struct GitPaths {
    /// The hooks dir git runs (honours `core.hooksPath`).
    hooks: PathBuf,
    toplevel: PathBuf,
    /// The shared git dir; its `hooks/` is where a hook goes without
    /// `core.hooksPath`, so a stale MUR block may be stranded there.
    common_dir: PathBuf,
}

fn ask_git(project_path: &Path) -> Option<GitPaths> {
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(project_path)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "hooks",
            "--show-toplevel",
            "--git-common-dir",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines().filter(|l| !l.is_empty()).map(PathBuf::from);
    Some(GitPaths {
        hooks: lines.next()?,
        toplevel: lines.next()?,
        common_dir: lines.next()?,
    })
}

// ─── Health: will the auto-index hook actually run? (#1672 point 3) ───

/// The line a user adds by hand to a versioned hooks dir (husky, lefthook).
pub(crate) const MANUAL_HOOK_CMD: &str = "mur project index --main-repo --quiet --background";

/// What makes a `post-commit` count as running MUR's auto-index: both the
/// installed block and [`MANUAL_HOOK_CMD`] contain it.
const HOOK_INVOCATION: &str = "project index";

/// The hook file git runs after a commit.
const POST_COMMIT: &str = "post-commit";

/// Whether MUR's auto-index hook will fire on the next commit. Read-only.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HookHealth {
    /// The `post-commit` git runs invokes `mur project index` and is executable.
    Active,
    /// The hooks dir git runs has no auto-index call. `in_work_tree`: the dir
    /// is versioned (e.g. `.husky/`), so the fix is a hand edit, not
    /// `mur project index`.
    NotInstalled {
        hooks_dir: PathBuf,
        in_work_tree: bool,
    },
    /// The silent #1672 failure: MUR's block sits in `hook`, but
    /// `core.hooksPath` sends git to `hooks_dir` instead.
    Stranded { hook: PathBuf, hooks_dir: PathBuf },
    /// The hook calls MUR but lacks the execute bit; git skips it silently.
    NotExecutable { hook: PathBuf },
    /// Not a git repo (or git is unavailable): there is no hook to check.
    NotARepo,
}

/// Check, without writing anything, whether auto-index will run on commit.
pub(crate) fn hook_health(project_path: &Path) -> HookHealth {
    let Some(git) = ask_git(project_path) else {
        // No git to ask: judge the legacy layout, as `resolve` does.
        let legacy = project_path.join(".git").join("hooks");
        if !legacy.is_dir() {
            return HookHealth::NotARepo;
        }
        return health_of(&legacy, false, None);
    };
    let in_tree = in_work_tree(&git.hooks, &git.toplevel);
    let default = git.common_dir.join("hooks");
    health_of(&git.hooks, in_tree, Some(&default))
}

fn health_of(hooks_dir: &Path, in_work_tree: bool, default_dir: Option<&Path>) -> HookHealth {
    let hook = hooks_dir.join(POST_COMMIT);
    let body = std::fs::read_to_string(&hook).unwrap_or_default();
    if body.contains(HOOK_INVOCATION) {
        return if is_executable(&hook) {
            HookHealth::Active
        } else {
            HookHealth::NotExecutable { hook }
        };
    }
    if let Some(default) = default_dir
        && !same_dir(default, hooks_dir)
    {
        let stale = default.join(POST_COMMIT);
        if std::fs::read_to_string(&stale).is_ok_and(|b| b.contains(super::HOOK_MARKER)) {
            return HookHealth::Stranded {
                hook: stale,
                hooks_dir: hooks_dir.to_path_buf(),
            };
        }
    }
    HookHealth::NotInstalled {
        hooks_dir: hooks_dir.to_path_buf(),
        in_work_tree,
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
