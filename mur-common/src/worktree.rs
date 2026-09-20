//! Git worktree identity: the link between a linked worktree and the main
//! checkout it belongs to (issue #004).
//!
//! Why this exists at all: entitlement checks are pure string prefix matching
//! (`tools::fs_policy::under_any`), and a `git worktree` lives at a path that
//! is NOT under the main checkout. So an agent granted the repo it works in
//! was refused the moment the work moved into a worktree, and the user had to
//! re-grant the same repo under a second name. Nothing in the runtime knew
//! the two paths were the same project.
//!
//! The derivation here is deliberately read-only and one-way: it computes the
//! relationship from git's own on-disk metadata every time it is asked. It
//! never writes a derived path back into `profile.yaml` — a grant the user did
//! not type must not become a permanent, user-visible entitlement they then
//! have to audit. The derived paths exist only in the built sandbox policy and
//! in the call-time tool gate.
//!
//! ## The on-disk shapes this reads
//!
//! Main checkout:      `<main>/.git/`                  — a DIRECTORY
//! Linked worktree:    `<wt>/.git`                     — a FILE containing
//!                                                       `gitdir: <main>/.git/worktrees/<id>`
//! Worktree registry:  `<main>/.git/worktrees/<id>/gitdir` — a file whose
//!                                                       contents are `<wt>/.git`
//!
//! Both directions are needed, for two different layers:
//!
//! * worktree → main ([`main_checkout_of`]) for the call-time tool gate, which
//!   sees a concrete path and asks "is this reachable from something granted?"
//! * main → worktrees ([`worktrees_of`]) for the kernel sandbox, which must
//!   enumerate every path at seal time because Landlock/SBPL cannot ask a
//!   question later.

use std::path::{Path, PathBuf};

/// Read a `.git` FILE's `gitdir: <path>` pointer. `None` when `p` is a
/// directory (an ordinary checkout), missing, or not in that form.
fn gitdir_pointer(p: &Path) -> Option<PathBuf> {
    // A `.git` directory is the main checkout — nothing to follow.
    if p.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(p).ok()?;
    let rest = text.trim().strip_prefix("gitdir:")?;
    let target = PathBuf::from(rest.trim());
    if target.as_os_str().is_empty() {
        return None;
    }
    // The pointer is normally absolute, but git permits a relative one
    // (`git worktree add --relative-paths`), resolved against the worktree.
    if target.is_absolute() {
        Some(target)
    } else {
        p.parent().map(|d| d.join(target))
    }
}

/// The main checkout that `start` belongs to, when `start` is inside a linked
/// git worktree. `None` for an ordinary checkout, or outside git entirely.
///
/// Resolution follows git's own two hops and nothing else — no `git` binary is
/// spawned, because this runs inside the entitlement gate that decides whether
/// spawning is allowed in the first place:
///
/// 1. `<wt>/.git` is a file → `gitdir: <main>/.git/worktrees/<id>`
/// 2. `<main>/.git/worktrees/<id>/commondir` → `../..`, joined and normalized
///    to `<main>/.git`, whose parent is the main checkout root.
///
/// `commondir` is read rather than assumed: it is the value git itself uses,
/// and it stays correct for layouts where the common dir is not two levels up
/// (a worktree of a bare or separate-gitdir repo).
pub fn main_checkout_of(start: &Path) -> Option<PathBuf> {
    let wt_root = worktree_root_of(start)?;
    let gitdir = gitdir_pointer(&wt_root.join(".git"))?;
    let commondir_file = gitdir.join("commondir");
    let common = match std::fs::read_to_string(&commondir_file) {
        Ok(text) => {
            let rel = PathBuf::from(text.trim());
            if rel.is_absolute() {
                rel
            } else {
                normalize(&gitdir.join(rel))
            }
        }
        // No `commondir` (very old git): fall back to the documented layout,
        // `<common>/worktrees/<id>` → up two.
        Err(_) => gitdir.parent()?.parent()?.to_path_buf(),
    };
    // `common` is the main repo's `.git`; the checkout is its parent. A bare
    // repo has no checkout to grant, so this correctly yields `None` only if
    // there is no parent at all.
    let root = common.parent()?.to_path_buf();
    if root.as_os_str().is_empty() {
        None
    } else {
        Some(root)
    }
}

/// Walk up from `start` to the nearest directory holding a `.git` entry of
/// either shape. Mirrors `project::repo_root_of` but is kept separate because
/// that function's contract (one project id per checkout) is deliberately
/// worktree-blind and callers depend on that.
fn worktree_root_of(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Every matching path in a linked worktree registered under the checkout that
/// contains `main_path`.
///
/// When `main_path` is the checkout root, this returns linked worktree roots.
/// When it is a descendant (for example `<main>/target`), the same relative
/// path is appended to every linked worktree root (for example
/// `<worktree>/target`). This lets every entitlement layer share one mapping
/// rule instead of reimplementing worktree-relative paths independently.
///
/// Reads `<main>/.git/worktrees/*/gitdir`, each of which contains the path of
/// the worktree's own `.git` file. Entries whose mapped path has been deleted
/// or was never created are skipped — a dead grant is not merely useless here,
/// it destabilizes the whole compiled sandbox profile (see
/// `sandbox::policy::from_entitlements`, Issue 16).
///
/// Returns empty for a path outside a main checkout, a linked-worktree path,
/// a checkout with no worktrees, or an unreadable registry. Never errors: an
/// undiscoverable worktree must degrade to "not granted" (fail-closed), never
/// to a panic inside the gate.
pub fn worktrees_of(main_path: &Path) -> Vec<PathBuf> {
    let Some(main_root) = worktree_root_of(main_path) else {
        return Vec::new();
    };
    // A `.git` file identifies a linked worktree. Expansion is deliberately
    // one-way from the main checkout so derived grants cannot fan out again.
    if !main_root.join(".git").is_dir() {
        return Vec::new();
    }
    let Ok(relative) = main_path.strip_prefix(&main_root) else {
        return Vec::new();
    };
    let dir = main_root.join(".git").join("worktrees");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let gitdir_file = entry.path().join("gitdir");
        let Ok(text) = std::fs::read_to_string(&gitdir_file) else {
            continue;
        };
        let dot_git = PathBuf::from(text.trim());
        if dot_git.as_os_str().is_empty() {
            continue;
        }
        let Some(root) = dot_git.parent() else {
            continue;
        };
        let mapped = root.join(relative);
        // Fail closed on a pruned/moved worktree or missing relative path.
        if std::fs::metadata(&mapped).is_ok() {
            out.push(mapped);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Lexically resolve `.` / `..` without touching the filesystem.
///
/// `std::fs::canonicalize` is not used on purpose: it resolves symlinks, and
/// the caller compares the result against entitlement roots the user typed by
/// hand. Rewriting `/Users/x/repo` into `/System/Volumes/Data/Users/x/repo`
/// (which macOS does) would make every derived grant miss.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Build a real repo with a real linked worktree. The metadata layout this
    /// module reads is git's, not ours, so a hand-faked fixture would only
    /// prove we can read what we wrote.
    fn repo_with_worktree() -> Option<(tempfile::TempDir, PathBuf, PathBuf)> {
        let tmp = tempfile::tempdir().ok()?;
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).ok()?;
        let git = |args: &[&str], cwd: &Path| -> bool {
            Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q"], &main) {
            return None; // no git on this machine → caller skips
        }
        let _ = git(&["config", "user.email", "t@example.com"], &main);
        let _ = git(&["config", "user.name", "t"], &main);
        std::fs::write(main.join("f.txt"), "x").ok()?;
        let _ = git(&["add", "f.txt"], &main);
        let _ = git(&["commit", "-qm", "init"], &main);
        let wt = tmp.path().join("wt");
        if !git(
            &["worktree", "add", "-q", wt.to_str()?, "-b", "feat"],
            &main,
        ) {
            return None;
        }
        Some((tmp, main, wt))
    }

    /// The bug in #004, stated as a property: a path inside a linked worktree
    /// must resolve back to the main checkout the user actually granted.
    #[test]
    fn worktree_path_resolves_to_its_main_checkout() {
        let Some((_tmp, main, wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable or worktree creation failed");
            return;
        };
        let main_c = std::fs::canonicalize(&main).unwrap();

        // From the worktree root...
        let got = main_checkout_of(&wt).expect("worktree resolves to a main checkout");
        assert_eq!(std::fs::canonicalize(&got).unwrap(), main_c);

        // ...and from a file nested deep inside it, which is what the file
        // tools actually receive.
        let deep = wt.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        let got = main_checkout_of(&deep.join("c.rs")).expect("nested path resolves");
        assert_eq!(std::fs::canonicalize(&got).unwrap(), main_c);
    }

    /// The reverse direction the kernel sandbox needs: from the granted main
    /// checkout, enumerate the worktrees to seal in alongside it.
    #[test]
    fn main_checkout_enumerates_its_worktrees() {
        let Some((_tmp, main, wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable or worktree creation failed");
            return;
        };
        let found = worktrees_of(&main);
        assert_eq!(found.len(), 1, "expected exactly one worktree: {found:?}");
        assert_eq!(
            std::fs::canonicalize(&found[0]).unwrap(),
            std::fs::canonicalize(&wt).unwrap()
        );
    }

    /// A pruned worktree must NOT be returned. Issue 16: a grant naming a
    /// nonexistent path destabilizes the compiled sandbox profile, so the
    /// derivation has to fail closed rather than pass the stale entry through.
    #[test]
    fn pruned_worktree_is_not_enumerated() {
        let Some((_tmp, main, wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable or worktree creation failed");
            return;
        };
        std::fs::remove_dir_all(&wt).unwrap();
        // Registry entry still present under .git/worktrees (not pruned).
        assert!(
            worktrees_of(&main).is_empty(),
            "a worktree deleted from disk must not be derived as a grant"
        );
    }

    /// An ordinary checkout is not a worktree: it must resolve to `None` so
    /// the gate falls through to the normal grant check unchanged.
    #[test]
    fn plain_checkout_and_non_repo_resolve_to_none() {
        let Some((_tmp, main, _wt)) = repo_with_worktree() else {
            eprintln!("skipping: git unavailable or worktree creation failed");
            return;
        };
        assert!(main_checkout_of(&main).is_none());

        let tmp2 = tempfile::tempdir().unwrap();
        assert!(main_checkout_of(tmp2.path()).is_none());
        assert!(worktrees_of(tmp2.path()).is_empty());
    }

    #[test]
    fn normalize_resolves_dotdot_without_the_filesystem() {
        assert_eq!(
            normalize(Path::new("/a/b/.git/worktrees/w/../..")),
            PathBuf::from("/a/b/.git")
        );
    }
}
