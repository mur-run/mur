//! Writable-but-not-deletable paths (#2582).
//!
//! The HITL classifier decides when to ask a human, and it matches text — an
//! interpreter can always spell a delete it does not recognise (#2581). This is
//! the layer that does not depend on spelling: the kernel refuses the unlink.
//!
//! * **macOS** — `(deny file-write-unlink (subpath …))`, emitted after every
//!   write allow (last-match-wins), so it layers on any grant.
//! * **Linux** — Landlock is additive: a rule on a parent hands its rights to
//!   every descendant, so `RemoveFile`/`RemoveDir` cannot be subtracted from a
//!   subtree. A write grant that reaches a no-delete path is split instead
//!   ([`split_no_delete_grants`]): the grant root and the directories on the
//!   way down lose `Remove*`, the siblings off that path keep everything. One
//!   cost follows from Landlock checking removal on the PARENT directory:
//!   files sitting directly in those intermediate directories cannot be
//!   deleted either.
//! * **Elsewhere** — no equivalent; launch refuses rather than run unenforced.
//!
//! What this does NOT prevent: truncation (`: > f` is a write) and anything
//! that replaces a file by renaming over it, which needs removal rights.

use std::path::{Component, Path, PathBuf};

use super::SandboxPolicy;

/// The path as written plus its symlink-resolved form.
fn forms(p: &Path) -> Vec<PathBuf> {
    let mut v = vec![p.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(p)
        && real != p
    {
        v.push(real);
    }
    v
}

/// Resolve `p` through its longest existing ancestor: `/data/alias/future`
/// with `alias -> real` and `future` not yet created becomes
/// `/data/real/future`. `canonicalize` alone fails on the whole entry, which
/// left the rule on a name the kernel never sees once the path appears
/// (review P1 on #2595). `None` when nothing resolves or a `..` component makes
/// the suffix ambiguous — the launch check refuses those.
fn resolve_through_ancestor(p: &Path) -> Option<PathBuf> {
    if p.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    let mut suffix = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(real) = std::fs::canonicalize(cur) {
            return Some(
                suffix
                    .iter()
                    .rev()
                    .fold(real, |acc: PathBuf, s| acc.join(s)),
            );
        }
        suffix.push(cur.file_name()?.to_os_string());
        cur = cur.parent()?;
    }
}

/// Each `no_delete` entry as written plus its resolved form, resolving a
/// missing entry through its longest existing ancestor.
pub(crate) fn resolve_no_delete(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = paths.clone();
    for p in &paths {
        if let Some(real) = resolve_through_ancestor(p)
            && real != *p
            && !out.contains(&real)
        {
            out.push(real);
        }
    }
    out
}

/// `true` when `p` is a no-delete path or an ancestor of one.
fn reaches(p: &Path, nd: &[PathBuf]) -> bool {
    forms(p).iter().any(|f| nd.iter().any(|n| n.starts_with(f)))
}

/// `true` when `p` lies inside (or is) a no-delete path.
fn inside(p: &Path, nd: &[PathBuf]) -> bool {
    forms(p).iter().any(|f| nd.iter().any(|n| f.starts_with(n)))
}

/// Write grants split for a backend that cannot subtract rights (Landlock).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct NoDeleteSplit {
    /// Grants that never meet a no-delete path: full write access.
    pub full: Vec<PathBuf>,
    /// Grant roots and carved directories on the way to a no-delete path, and
    /// grants lying inside one: write access minus `RemoveFile`/`RemoveDir`.
    pub no_remove: Vec<PathBuf>,
}

/// Split `grants` so no rule carrying removal rights covers a no-delete path.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn split_no_delete_grants(grants: &[PathBuf], nd: &[PathBuf]) -> NoDeleteSplit {
    let mut split = NoDeleteSplit::default();
    if nd.is_empty() {
        split.full = grants.to_vec();
        return split;
    }
    let mut seen = Vec::new();
    for g in grants {
        if inside(g, nd) {
            push_once(&mut split.no_remove, g);
        } else if reaches(g, nd) {
            push_once(&mut split.no_remove, g);
            carve(g, nd, &mut seen, &mut split);
        } else {
            push_once(&mut split.full, g);
        }
    }
    split
}

fn push_once(v: &mut Vec<PathBuf>, p: &Path) {
    if !v.iter().any(|x| x == p) {
        v.push(p.to_path_buf());
    }
}

/// Open `dir` one level: entries off the path to a no-delete path keep full
/// rights, the no-delete path itself and the directories above one are
/// recorded as `no_remove` (opened again in turn). Fail-closed: a directory
/// that cannot be read grants its children nothing beyond the parent's
/// no-remove rule.
fn carve(dir: &Path, nd: &[PathBuf], seen: &mut Vec<PathBuf>, out: &mut NoDeleteSplit) {
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if seen.contains(&real) {
        return;
    }
    seen.push(real);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if inside(&p, nd) {
            // The no-delete path itself: the parent's no-remove rule already
            // covers it; an explicit rule keeps the intent visible.
            push_once(&mut out.no_remove, &p);
        } else if reaches(&p, nd) {
            if p.is_dir() {
                push_once(&mut out.no_remove, &p);
                carve(&p, nd, seen, out);
            }
        } else {
            push_once(&mut out.full, &p);
        }
    }
}

impl SandboxPolicy {
    /// Refuse a seal that would leave a `no_delete` entry unenforced.
    ///
    /// Fail-closed (#2582): an entry the kernel silently ignores is worse than
    /// a launch error, because the user believes the files are protected.
    pub(crate) fn check_no_delete_enforceable(&self) -> anyhow::Result<()> {
        if self.fs_no_delete.is_empty() {
            return Ok(());
        }
        if !NO_DELETE_SUPPORTED {
            anyhow::bail!(
                "filesystem.no_delete is set but this platform's sandbox cannot \
                 enforce it (supported: macOS, Linux). Remove the entries with \
                 `mur agent perm remove-path <agent> no_delete <path>` to start."
            );
        }
        // A `..` after a symlink names a different directory than the text
        // suggests; refuse rather than guess which tree the user meant.
        if let Some(n) = self
            .fs_no_delete
            .iter()
            .find(|n| n.components().any(|c| c == Component::ParentDir))
        {
            anyhow::bail!(
                "filesystem.no_delete entry {} contains `..`; write the path \
                 without it so the protected tree is unambiguous.",
                n.display()
            );
        }
        // Linux only: splitting the agent's own home would strip removal from
        // its top level, where the runtime replaces its own files by
        // temp + rename. macOS layers a deny clause and needs no split.
        if cfg!(target_os = "linux") {
            let home = self.launch_chain.agent_self_home();
            if let Some(n) = self.fs_no_delete.iter().find(|n| n.starts_with(home)) {
                anyhow::bail!(
                    "filesystem.no_delete entry {} lies inside the agent's own \
                     home {}; Landlock would have to strip delete rights from the \
                     home itself, which the runtime needs. Move the protected \
                     directory outside the agent home.",
                    n.display(),
                    home.display()
                );
            }
        }
        Ok(())
    }

    /// `no_delete` entries outside every write grant: the path is not
    /// writable at all, so the entry changes nothing.
    pub fn ineffective_no_delete(&self) -> Vec<PathBuf> {
        self.fs_no_delete
            .iter()
            .filter(|n| {
                !self
                    .fs_write
                    .iter()
                    .any(|w| inside(n, std::slice::from_ref(w)))
            })
            .cloned()
            .collect()
    }
}

/// Whether this platform's kernel sandbox can enforce `no_delete`.
pub const NO_DELETE_SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "linux"));

#[cfg(test)]
#[path = "no_delete_tests.rs"]
mod tests;
