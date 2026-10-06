//! Per-turn tracks: a git worktree whose working tree is a clone of the
//! project working tree, *including* its uncommitted state.
//!
//! The shape is the same on every host (design spec §4.1): `.git` inside the
//! track is the usual worktree pointer file, so the object store, refs and
//! branches are shared with the project and nothing the agent commits is lost
//! when the track goes. Hosts differ only in how the working tree is cloned
//! ([`TreeClone`]).
//!
//! `diff_files` compares content, not `git status` lines: a file that was
//! already dirty when the turn began is reported only if the turn changed it
//! further, and a file the turn reverted to clean is still reported, so
//! `promote` carries that reversal into the project.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[cfg(test)]
mod tests;

/// Where tracks live, under the project root. Shared with the fleet backend
/// so one `.gitignore` line covers both.
pub const WORKTREES_DIR: &str = ".worktrees";

/// Written into the track's private git dir (`.git/worktrees/<name>/`), never
/// into the working tree, so it can't show up as a changed file.
const BASE_FILE: &str = "mur-turn-base.json";

/// Directory names never cloned into a track, at any depth. `.git` because
/// the pointer file replaces it; the rest because they are build output or
/// other tracks, large and regenerable.
pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", WORKTREES_DIR];

/// How the project working tree is cloned into the track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeClone {
    /// Plain recursive copy. Always available; cost is linear in bytes.
    Copy,
    /// macOS APFS `clonefile(2)` per entry: metadata-only, blocks are shared
    /// until written. Requires track and project on one APFS volume —
    /// `.worktrees/` under the project guarantees that. Falls back to a byte
    /// copy per file when the kernel refuses (non-APFS volume, special file).
    #[cfg(target_os = "macos")]
    ApfsClone,
}

impl TreeClone {
    /// The best clone method for this host.
    pub fn detect() -> Self {
        #[cfg(target_os = "macos")]
        return TreeClone::ApfsClone;
        #[cfg(not(target_os = "macos"))]
        TreeClone::Copy
    }
}

/// Content fingerprint of every path that was dirty or untracked at turn
/// start, keyed by path relative to the project root. `None` = absent on disk
/// (a staged deletion).
type BaseState = BTreeMap<PathBuf, Option<String>>;

#[derive(Serialize, Deserialize)]
struct Sentinel {
    project: PathBuf,
    base: BaseState,
}

/// One turn's track. Create it before the model sees the prompt, hand the
/// model [`TurnTrack::path`] as its working directory, then `diff_files` /
/// `promote` / `destroy` at turn end.
#[derive(Debug)]
pub struct TurnTrack {
    project: PathBuf,
    path: PathBuf,
    base: BaseState,
}

impl TurnTrack {
    /// Create `<project>/.worktrees/<name>` as a detached worktree and clone
    /// the project working tree into it. `project` must be the repository
    /// root; `name` is restricted to the fleet-name alphabet so it can never
    /// escape `.worktrees/`.
    pub fn create(project: &Path, name: &str, clone: TreeClone) -> Result<Self> {
        if !mur_common::fleet::valid_fleet_name(name) {
            bail!("invalid track name '{name}': use lowercase letters, digits, '-' or '_'");
        }
        let project = repo_root(project)?;
        let path = project.join(WORKTREES_DIR).join(name);
        if path.exists() {
            bail!("track already exists: {}", path.display());
        }
        ensure_excluded(&project)?;
        // The base is captured BEFORE the clone so a file the user saves
        // between the two is seen as the turn's change (and promoted back),
        // never silently dropped.
        let base = fingerprint_dirty(&project)?;
        run_git(
            &project,
            &["worktree", "add", "--detach", "--no-checkout", "-q"],
            Some(&path),
        )
        .context("git worktree add")?;
        let git_dir = absolute_git_dir(&path)?;
        let project_git_dir = absolute_git_dir(&project)?;
        // The index is what makes `git status` in the track agree with the
        // project: without it every tracked file reads as deleted-and-new.
        let project_index = project_git_dir.join("index");
        if project_index.exists() {
            std::fs::copy(&project_index, git_dir.join("index")).context("copy index")?;
        }
        clone_tree(&project, &path, clone)?;
        let sentinel = Sentinel {
            project: project.clone(),
            base: base.clone(),
        };
        std::fs::write(git_dir.join(BASE_FILE), serde_json::to_vec(&sentinel)?)
            .context("write turn base")?;
        Ok(Self {
            project,
            path,
            base,
        })
    }

    /// Re-attach to a track created earlier (another process, or a turn that
    /// was kept for review).
    pub fn open(project: &Path, name: &str) -> Result<Self> {
        if !mur_common::fleet::valid_fleet_name(name) {
            bail!("invalid track name '{name}'");
        }
        let project = repo_root(project)?;
        let path = project.join(WORKTREES_DIR).join(name);
        let git_dir = absolute_git_dir(&path)?;
        let raw = std::fs::read(git_dir.join(BASE_FILE))
            .with_context(|| format!("no turn base in {}: not a turn track?", path.display()))?;
        let sentinel: Sentinel = serde_json::from_slice(&raw).context("parse turn base")?;
        Ok(Self {
            project,
            path,
            base: sentinel.base,
        })
    }

    /// The track's working tree — what the model is handed as its cwd.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The project root this track shadows.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// A path inside the track, mapped back to where it will land in the
    /// project. Paths outside the track are returned unchanged.
    pub fn display_path(&self, p: &Path) -> PathBuf {
        match p.strip_prefix(&self.path) {
            Ok(rel) => self.project.join(rel),
            Err(_) => p.to_path_buf(),
        }
    }

    /// Files the turn changed, relative to the project root. Content-based:
    /// pre-existing dirt is excluded, a change that reverts to clean is not.
    pub fn diff_files(&self) -> Result<Vec<PathBuf>> {
        let now = fingerprint_dirty(&self.path)?;
        let mut out = Vec::new();
        for (rel, hash) in &now {
            if self.base.get(rel) != Some(hash) {
                out.push(rel.clone());
            }
        }
        for (rel, base_hash) in &self.base {
            if !now.contains_key(rel) {
                // Clean in the track now. Changed by the turn unless it is
                // byte-identical to what the base recorded.
                let current = fingerprint_path(&self.path.join(rel))?;
                if &current != base_hash {
                    out.push(rel.clone());
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Copy the turn's changes over the project working tree — last write
    /// wins, deletions propagate. Returns what was promoted (relative paths).
    /// `.git` is untouched: commits are already in the shared store.
    pub fn promote(&self) -> Result<Vec<PathBuf>> {
        let files = self.diff_files()?;
        for rel in &files {
            let src = self.path.join(rel);
            let dst = self.project.join(rel);
            match std::fs::symlink_metadata(&src) {
                Ok(_) => {
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Replace, don't write-through: a symlink in the project
                    // must not have the track's bytes poured into its target.
                    if std::fs::symlink_metadata(&dst).is_ok() {
                        remove_any(&dst)?;
                    }
                    copy_entry(&src, &dst)?;
                }
                Err(_) => {
                    if std::fs::symlink_metadata(&dst).is_ok() {
                        remove_any(&dst)?;
                    }
                }
            }
        }
        Ok(files)
    }

    /// Remove the worktree and its registration. Branches and commits made
    /// in the track survive; a detached-HEAD commit without a branch does not
    /// (its reflog lives in the worktree's git dir).
    pub fn destroy(self) -> Result<()> {
        let removed = run_git(
            &self.project,
            &["worktree", "remove", "--force"],
            Some(&self.path),
        );
        if removed.is_err() {
            // The dir may already be gone (user deleted it); drop the stale
            // registration so the name can be reused.
            if self.path.exists() {
                std::fs::remove_dir_all(&self.path)?;
            }
            run_git(&self.project, &["worktree", "prune"], None).context("git worktree prune")?;
        }
        Ok(())
    }
}

/// Make git ignore `.worktrees/` for this repository without touching the
/// user's `.gitignore`: `.git/info/exclude` is repo-local and never
/// committed. Idempotent.
fn ensure_excluded(project: &Path) -> Result<()> {
    let info = absolute_git_dir(project)?.join("info");
    let exclude = info.join("exclude");
    let line = format!("/{WORKTREES_DIR}/");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    std::fs::create_dir_all(&info)?;
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    std::fs::write(&exclude, text).context("write .git/info/exclude")?;
    Ok(())
}

/// `std::fs::canonicalize` minus the Windows verbatim prefix. On Windows the
/// std call yields `\\?\C:\...`; git refuses that shape (`could not create
/// leading directories of '//?/C:/...'`) and the shell never shows it to the
/// model, so every path a track hands out or compares goes through here.
/// Elsewhere it is exactly `std::fs::canonicalize`.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(strip_verbatim)
}

#[cfg(windows)]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    use std::path::Prefix;
    let mut comps = p.components();
    let Some(Component::Prefix(pre)) = comps.next() else {
        return p;
    };
    let head = match pre.kind() {
        Prefix::VerbatimDisk(d) => format!("{}:\\", d as char),
        Prefix::VerbatimUNC(server, share) => format!(
            "\\\\{}\\{}\\",
            server.to_string_lossy(),
            share.to_string_lossy()
        ),
        _ => return p,
    };
    let mut out = PathBuf::from(head);
    for c in comps {
        if let Component::Normal(n) = c {
            out.push(n);
        }
    }
    out
}

#[cfg(not(windows))]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Canonical repository root of `dir`, or an error naming the problem.
fn repo_root(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .context("spawn git rev-parse")?;
    if !out.status.success() {
        bail!("not a git repository: {}", dir.display());
    }
    let top = PathBuf::from(String::from_utf8(out.stdout)?.trim());
    let top = canonicalize(&top).unwrap_or(top);
    let dir = canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if top != dir {
        bail!(
            "turn tracks are created at the repository root ({}), not {}",
            top.display(),
            dir.display()
        );
    }
    Ok(top)
}

fn absolute_git_dir(worktree: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(worktree)
        .output()
        .context("spawn git rev-parse --absolute-git-dir")?;
    if !out.status.success() {
        bail!(
            "not a git worktree: {} ({})",
            worktree.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

fn run_git(cwd: &Path, args: &[&str], path_arg: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    if let Some(p) = path_arg {
        cmd.arg(p);
    }
    let out = cmd
        .output()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Every path `git status` considers dirty or untracked under `root`, with
/// its content fingerprint. Ignored files are invisible by construction, so
/// build output never counts as a change.
fn fingerprint_dirty(root: &Path) -> Result<BaseState> {
    let out = Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ])
        .current_dir(root)
        .output()
        .context("spawn git status")?;
    if !out.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut state = BaseState::new();
    for entry in out.stdout.split(|b| *b == 0) {
        // `XY path` — two status columns, a space, then the path.
        if entry.len() < 4 {
            continue;
        }
        let rel = PathBuf::from(String::from_utf8_lossy(&entry[3..]).into_owned());
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            continue;
        }
        // Never cloned, so never part of the turn: a project that does not
        // ignore `target/` would otherwise look like the turn deleted it.
        if rel.components().any(|c| match c {
            Component::Normal(n) => SKIP_DIRS.iter().any(|s| n == *s),
            _ => false,
        }) {
            continue;
        }
        let hash = fingerprint_path(&root.join(&rel))?;
        state.insert(rel, hash);
    }
    Ok(state)
}

/// SHA-256 of a file's bytes (or a symlink's target); `None` when absent.
fn fingerprint_path(p: &Path) -> Result<Option<String>> {
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("stat {}", p.display())),
    };
    let mut h = Sha256::new();
    if meta.file_type().is_symlink() {
        h.update(b"link:");
        h.update(std::fs::read_link(p)?.as_os_str().as_encoded_bytes());
    } else if meta.is_dir() {
        // `--untracked-files=all` never lists directories; be safe anyway.
        h.update(b"dir");
    } else {
        h.update(std::fs::read(p).with_context(|| format!("read {}", p.display()))?);
    }
    Ok(Some(format!("{:x}", h.finalize())))
}

/// Clone `src`'s working tree into `dst` (which already holds the `.git`
/// pointer), skipping [`SKIP_DIRS`] at every depth.
fn clone_tree(src: &Path, dst: &Path, method: TreeClone) -> Result<()> {
    let leaf: &dyn Fn(&Path, &Path) -> Result<()> = match method {
        TreeClone::Copy => &copy_entry,
        #[cfg(target_os = "macos")]
        TreeClone::ApfsClone => &apfs::clone_entry,
    };
    walk_tree(src, dst, leaf)
}

/// Recreate `src`'s directory structure under `dst`, calling `leaf` for every
/// non-directory entry and skipping [`SKIP_DIRS`] at every depth. Directories
/// are walked rather than cloned whole so a nested `target/` is skipped too.
fn walk_tree(src: &Path, dst: &Path, leaf: &dyn Fn(&Path, &Path) -> Result<()>) -> Result<()> {
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_DIRS.iter().any(|s| name == *s) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&to)?;
            walk_tree(&from, &to, leaf)?;
        } else {
            leaf(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod apfs {
    use super::copy_entry;
    use anyhow::Result;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`; `libc` does not export it.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    /// `clonefile(2)` one entry; a symlink is cloned as a symlink
    /// (`CLONE_NOFOLLOW`). Any refusal degrades to a byte copy of that one
    /// entry, so a stray non-APFS mount or special file never fails the turn.
    pub(super) fn clone_entry(from: &Path, to: &Path) -> Result<()> {
        if try_clonefile(from, to) {
            return Ok(());
        }
        copy_entry(from, to)
    }

    /// True when the kernel cloned `from` to `to`; false means the caller
    /// must copy bytes.
    pub(super) fn try_clonefile(from: &Path, to: &Path) -> bool {
        let (Ok(src), Ok(dst)) = (
            CString::new(from.as_os_str().as_bytes()),
            CString::new(to.as_os_str().as_bytes()),
        ) else {
            return false;
        };
        // SAFETY: both pointers are valid NUL-terminated paths for the call.
        unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) == 0 }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn clonefile_is_really_used_on_this_volume() {
            let td = tempfile::tempdir().unwrap();
            let from = td.path().join("a");
            std::fs::write(&from, "bytes").unwrap();
            assert!(
                super::try_clonefile(&from, &td.path().join("b")),
                "clonefile(2) refused on the temp volume — the fast path is dead here"
            );
            assert_eq!(
                std::fs::read_to_string(td.path().join("b")).unwrap(),
                "bytes"
            );
        }
    }
}

/// Copy one non-directory entry, preserving symlinks as symlinks.
fn copy_entry(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to)
            .with_context(|| format!("symlink {}", to.display()))?;
        #[cfg(not(unix))]
        bail!("symlinks in a turn track are unsupported on this platform");
    } else {
        std::fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    }
    Ok(())
}

fn remove_any(p: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(p)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(p)?;
    } else {
        std::fs::remove_file(p)?;
    }
    Ok(())
}
