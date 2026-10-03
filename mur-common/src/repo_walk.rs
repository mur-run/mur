//! The one upward walk to the nearest `.git`, bounded by the agent scratch
//! tree.
//!
//! `<mur_home>` is commonly itself a git repo (the versioned store), and every
//! agent's scratch dir lives at `<mur_home>/<SCRATCH_ROOT>/<agent>`. An
//! unbounded walk from inside a scratch dir therefore always lands on
//! `<mur_home>`, so project instructions, project-scoped skills, delegation
//! routing and worktree grants all silently pointed at MUR's own store.
//! Linux CI never sees it (its `TMPDIR` is `/tmp`), which is why the bound
//! lives here, in the single helper every walker shares, rather than at each
//! call site.

use std::path::{Path, PathBuf};

/// Directory name under `<mur_home>` holding every agent's scratch dir.
/// `mur-agent-runtime::agent_paths` re-exports this, so the walk bound and
/// the directory the runtime creates can never name different places.
pub const SCRATCH_ROOT: &str = "tmp";

/// `<mur_home>/<SCRATCH_ROOT>` for the current `MUR_HOME` (or `~/.mur`).
pub fn scratch_root() -> PathBuf {
    crate::trust::mur_home().join(SCRATCH_ROOT)
}

/// Every directory a repo walk must not climb out of: the scratch root, and
/// the process temp dir. Inside an agent child the two coincide (`TMPDIR` is
/// the agent's scratch dir); elsewhere the temp dir bound still holds, because
/// a temp dir is never part of an enclosing project, and it does not depend
/// on `MUR_HOME` (which tests and wrappers routinely point elsewhere).
pub fn walk_ceilings() -> Vec<PathBuf> {
    vec![scratch_root(), std::env::temp_dir()]
}

/// Walk up from `start` to the nearest directory holding a `.git` entry (file
/// or directory), never entering any of `ceilings`. A repo found strictly
/// below a ceiling still counts — an agent may `git init` inside its scratch
/// dir.
///
/// Ceilings are matched both as given and canonicalized, because callers pass
/// either form (the file-tool gate hands in canonical paths, and macOS
/// rewrites `/var` to `/private/var`).
pub fn git_root_bounded<P: AsRef<Path>>(start: &Path, ceilings: &[P]) -> Option<PathBuf> {
    let stops: Vec<PathBuf> = ceilings
        .iter()
        .flat_map(|c| {
            let c = c.as_ref();
            [Some(c.to_path_buf()), std::fs::canonicalize(c).ok()]
        })
        .flatten()
        .collect();
    let is_stop = |d: &Path| stops.iter().any(|s| s == d);
    let mut dir = Some(start);
    while let Some(d) = dir {
        if is_stop(d) {
            return None;
        }
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// [`git_root_bounded`] with the live [`walk_ceilings`].
pub fn git_root_of(start: &Path) -> Option<PathBuf> {
    git_root_bounded(start, &walk_ceilings())
}

/// Env var name git reads to bound its own upward repo discovery.
pub const GIT_CEILING_ENV: &str = "GIT_CEILING_DIRECTORIES";

/// The same bound for code that spawns `git` instead of walking itself
/// (`git rev-parse --show-toplevel` and friends): set this on the `Command`
/// so git stops at the same [`walk_ceilings`] as [`git_root_of`]. Any
/// ceiling the caller's environment already set is kept.
pub fn bound_git(cmd: &mut std::process::Command) -> &mut std::process::Command {
    bound_git_at(cmd, &walk_ceilings())
}

/// [`bound_git`] with explicit ceilings — the env-free seam tests use, since
/// mutating `MUR_HOME` races every parallel test that reads it.
pub fn bound_git_at<'a, P: AsRef<Path>>(
    cmd: &'a mut std::process::Command,
    ceilings: &[P],
) -> &'a mut std::process::Command {
    let inherited = std::env::var_os(GIT_CEILING_ENV);
    let mut all: Vec<PathBuf> = inherited
        .as_deref()
        .map(|v| std::env::split_paths(v).collect())
        .unwrap_or_default();
    all.extend(ceilings.iter().map(|c| c.as_ref().to_path_buf()));
    match std::env::join_paths(&all) {
        Ok(v) => cmd.env(GIT_CEILING_ENV, v),
        // A ceiling containing the separator cannot be expressed; leave git
        // unbounded rather than hand it a wrong list.
        Err(_) => cmd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bound_itself_and_everything_above_it_are_unreachable() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".git")).unwrap();
        let stop = home.join(SCRATCH_ROOT);
        fs::create_dir_all(stop.join("a")).unwrap();

        assert_eq!(git_root_bounded(&stop.join("a"), &[&stop]), None);
        assert_eq!(git_root_bounded(&stop, &[&stop]), None);
        assert_eq!(git_root_bounded(&home, &[&stop]), Some(home));
    }

    /// The production shape: `<mur_home>` is a git repo and the agent works
    /// in `<mur_home>/tmp/<agent>/...`. A repo the agent creates inside its
    /// scratch dir still counts.
    #[test]
    fn scratch_dir_under_a_git_tracked_home_is_not_in_a_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".git")).unwrap();
        let stop = home.join(SCRATCH_ROOT);
        let job = stop.join("agent").join("job");
        let inner = stop.join("agent").join("proj");
        fs::create_dir_all(&job).unwrap();
        fs::create_dir_all(inner.join(".git")).unwrap();
        fs::create_dir_all(inner.join("src")).unwrap();

        assert_eq!(git_root_bounded(&job, &[&stop]), None);
        assert_eq!(git_root_bounded(&inner.join("src"), &[&stop]), Some(inner));
    }

    /// Same bound, enforced by git itself for callers that spawn
    /// `git rev-parse` instead of walking.
    #[test]
    fn spawned_git_stops_at_the_same_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let git = |dir: &Path, bound: Option<&Path>| {
            let mut cmd = std::process::Command::new("git");
            if let Some(b) = bound {
                bound_git_at(&mut cmd, &[b]);
            }
            cmd.arg("-C")
                .arg(dir)
                .args(["rev-parse", "--show-toplevel"]);
            cmd.output().ok().map(|o| o.status.success())
        };
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&home)
            .status();
        if !init.is_ok_and(|s| s.success()) {
            eprintln!("skipping: git unavailable");
            return;
        }
        let stop = home.join(SCRATCH_ROOT);
        let job = stop.join("agent").join("job");
        fs::create_dir_all(&job).unwrap();

        assert_eq!(git(&job, None), Some(true), "unbounded git escapes");
        assert_eq!(git(&job, Some(&stop)), Some(false));
        assert_eq!(git(&home, Some(&stop)), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_start_still_honours_a_symlinked_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join(".git")).unwrap();
        fs::create_dir_all(real.join(SCRATCH_ROOT).join("a")).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let start = fs::canonicalize(real.join(SCRATCH_ROOT).join("a")).unwrap();
        assert_eq!(git_root_bounded(&start, &[link.join(SCRATCH_ROOT)]), None);
    }
}
