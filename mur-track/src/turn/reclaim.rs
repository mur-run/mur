//! Reclaiming turn tracks nobody will close (#2152).
//!
//! A track normally goes away when its turn closes it. Three paths leave one
//! behind: a refused promote (`kept`), a promote whose `destroy` failed, and a
//! runtime that died mid-turn. Nothing used to come back for them, so they
//! piled up under `.worktrees/`.
//!
//! Ownership is recorded in the track's private git dir (never the working
//! tree, so it can't show up as a changed file): the pid of the process that
//! opened it, and whether that process has let it go. A track is reclaimable
//! only when its owner is dead or released it AND it has been quiet for the
//! grace period. A track another live process still owns is never touched —
//! that is how a concurrent session on the same project stays safe.
//!
//! Reclaiming saves the track's whole diff as a patch first and removes the
//! worktree only when that write succeeded, so the only copy of a turn's
//! edits is never the thing deleted.

use super::{BASE_FILE, WORKTREES_DIR, common_git_dir, run_git};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Ownership record, beside [`BASE_FILE`] in the track's git dir.
pub(super) const OWNER_FILE: &str = "mur-turn-owner.json";

/// Tracks written before owner files existed have no pid to ask, so they are
/// only reclaimed once older than this — far beyond any real turn.
pub const LEGACY_GRACE: Duration = Duration::from_secs(24 * 60 * 60);

/// Scratch index used to stage the whole track for the patch.
const PATCH_INDEX: &str = "mur-reclaim-index";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Owner {
    pub pid: u32,
    #[serde(default)]
    pub released: bool,
}

pub(super) fn write_owner(git_dir: &Path, owner: &Owner) -> Result<()> {
    let path = git_dir.join(OWNER_FILE);
    let tmp = git_dir.join(format!("{OWNER_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(owner)?).context("write turn owner")?;
    std::fs::rename(&tmp, &path).context("commit turn owner")?;
    Ok(())
}

fn read_owner(git_dir: &Path) -> Option<Owner> {
    serde_json::from_slice(&std::fs::read(git_dir.join(OWNER_FILE)).ok()?).ok()
}

/// Why a track was judged reclaimable — logged, and useful in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// Its owner closed it (kept, or promoted but not destroyed).
    Released,
    /// Its owner process is gone (crash, kill, restart).
    OwnerDead,
    /// No owner file: written by an older runtime, older than [`LEGACY_GRACE`].
    Legacy,
}

/// A turn track nobody owns any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTrack {
    pub name: String,
    pub path: PathBuf,
    /// The track's private git dir (`<common>/worktrees/<name>`).
    pub git_dir: PathBuf,
    pub reason: StaleReason,
}

/// Every turn track under `<project>/.worktrees/` that is safe to reclaim.
///
/// A directory counts as a turn track only when git has it registered as a
/// worktree of `project` and its git dir holds the turn base file — fleet
/// worktrees and stray dirs are never candidates. The git dir is located
/// through the project, not through the track's `.git` pointer: a gutted
/// track may have lost that pointer, and git run inside it would then answer
/// for the project repo.
pub fn stale_tracks(project: &Path, grace: Duration) -> Result<Vec<StaleTrack>> {
    let root = project.join(WORKTREES_DIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let admin = common_git_dir(project)?.join("worktrees");
    let now = SystemTime::now();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = entry.path();
        let git_dir = admin.join(&name);
        if !registered_at(&git_dir, &path) || !git_dir.join(BASE_FILE).is_file() {
            continue;
        }
        if let Some(reason) = judge(&git_dir, now, grace) {
            out.push(StaleTrack {
                name,
                path,
                git_dir,
                reason,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Does git's admin dir `git_dir` belong to the worktree at `path`? Guards
/// against a name collision where git suffixed the admin dir.
fn registered_at(git_dir: &Path, path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(git_dir.join("gitdir")) else {
        return false;
    };
    let recorded = PathBuf::from(raw.trim());
    let canon = |p: &Path| super::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let expected = path.join(".git");
    recorded == expected || canon(&recorded) == canon(&expected)
}

fn judge(git_dir: &Path, now: SystemTime, grace: Duration) -> Option<StaleReason> {
    let (reason, stamp, wait) = match read_owner(git_dir) {
        Some(o) if o.released => (StaleReason::Released, OWNER_FILE, grace),
        Some(o) if !mur_common::lock_file::pid_alive(o.pid) => {
            (StaleReason::OwnerDead, OWNER_FILE, grace)
        }
        Some(_) => return None,
        None => (StaleReason::Legacy, BASE_FILE, grace.max(LEGACY_GRACE)),
    };
    let modified = std::fs::metadata(git_dir.join(stamp))
        .and_then(|m| m.modified())
        .ok()?;
    // A clock that went backwards reads as "just now": wait, don't delete.
    let age = now.duration_since(modified).unwrap_or_default();
    (age >= wait).then_some(reason)
}

/// Save `track`'s diff against its HEAD, then remove the worktree and its
/// registration. Added and modified files go to `patch` (`git apply`-able);
/// deleted paths are listed one per line in [`deleted_list`]`(patch)` —
/// their bytes are in the commit already, and a gutted track would otherwise
/// produce a patch the size of the repo. An empty diff still writes an
/// (empty) patch, so the caller can always say where the track went.
pub fn reclaim(project: &Path, track: &StaleTrack, patch: &Path) -> Result<()> {
    save_patch(track, patch)?;
    let removed = run_git(
        project,
        &["worktree", "remove", "--force", "--force"],
        Some(&track.path),
    );
    if removed.is_err() {
        if track.path.exists() {
            std::fs::remove_dir_all(&track.path)
                .with_context(|| format!("remove {}", track.path.display()))?;
        }
        run_git(project, &["worktree", "prune"], None).context("git worktree prune")?;
    }
    Ok(())
}

fn save_patch(track: &StaleTrack, patch: &Path) -> Result<()> {
    let index = track.git_dir.join(PATCH_INDEX);
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("--git-dir")
            .arg(&track.git_dir)
            .arg("--work-tree")
            .arg(&track.path)
            .args(args)
            .current_dir(&track.path)
            .env("GIT_INDEX_FILE", &index)
            .output()
    };
    let src = track.git_dir.join("index");
    if src.is_file() {
        std::fs::copy(&src, &index).context("copy track index")?;
    }
    let result = (|| -> Result<(Vec<u8>, Vec<u8>)> {
        let added = git(&["add", "-A", "--", "."]).context("spawn git add")?;
        if !added.status.success() {
            bail!(
                "git add failed: {}",
                String::from_utf8_lossy(&added.stderr).trim()
            );
        }
        let diff = |args: &[&str]| -> Result<Vec<u8>> {
            let out = git(args).context("spawn git diff")?;
            if !out.status.success() {
                bail!(
                    "git diff failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(out.stdout)
        };
        let changes = diff(&["diff", "--cached", "--binary", "--diff-filter=d", "HEAD"])?;
        let deleted = diff(&["diff", "--cached", "--name-only", "--diff-filter=D", "HEAD"])?;
        Ok((changes, deleted))
    })();
    let _ = std::fs::remove_file(&index);
    let (changes, deleted) = result?;
    if let Some(parent) = patch.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(patch, changes).with_context(|| format!("write {}", patch.display()))?;
    if !deleted.is_empty() {
        let list = deleted_list(patch);
        std::fs::write(&list, deleted).with_context(|| format!("write {}", list.display()))?;
    }
    Ok(())
}

/// Where [`reclaim`] lists the paths the track deleted, beside `patch`.
pub fn deleted_list(patch: &Path) -> PathBuf {
    patch.with_extension("deleted.txt")
}
