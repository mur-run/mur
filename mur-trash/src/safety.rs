//! Path safety (D5): what may enter the trash, and what `trash empty|now` may
//! delete. Live turn tracks are `bash_guard`'s job and are not checked here.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::layout::trash_root;
use crate::mover::remove_any;

/// Why a target is not moved into the trash.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `/` itself.
    FilesystemRoot,
    /// The agent home, or a directory that contains it.
    AgentHome,
    /// The trash root or anything inside it (it would nest).
    InsideTrash,
    /// No final name to file it under (`.`, `..`, empty).
    NoFileName,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::FilesystemRoot => "refusing to trash the filesystem root",
            Self::AgentHome => "refusing to trash the agent home or a directory containing it",
            Self::InsideTrash => "path is inside the trash",
            Self::NoFileName => "path has no file name",
        })
    }
}

/// Resolve `path` the way `rm` sees it: parent canonicalized, final component
/// kept as-is so a symlink is the link, not its target.
pub fn resolve_no_follow(path: &Path) -> io::Result<PathBuf> {
    let name = match path.components().next_back() {
        Some(Component::Normal(n)) => n.to_owned(),
        _ => {
            // `/`, `.`, `..`: no link to preserve, canonicalize the whole thing.
            return path.canonicalize();
        }
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.canonicalize()?,
        _ => std::env::current_dir()?,
    };
    Ok(parent.join(name))
}

/// Canonical form of a path that may not exist yet (the trash root before the
/// first delete): canonicalize the nearest existing ancestor, re-append the rest.
fn canonical_lenient(path: &Path) -> io::Result<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        match cur.canonicalize() {
            Ok(base) => return Ok(rest.iter().rev().fold(base, |p, c| p.join(c))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) else {
                    return Err(e);
                };
                rest.push(name.to_owned());
                cur = parent;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Check `target` may be moved into `agent_home`'s trash.
///
/// `Err(io)` means the target could not be resolved (usually missing); the
/// caller applies the remover's own missing-path rule (`-f`).
pub fn check_target(target: &Path, agent_home: &Path) -> io::Result<Result<PathBuf, Refusal>> {
    let resolved = resolve_no_follow(target)?;
    // The parent resolved, but the final component may not exist. A dangling
    // link does exist (`rm` deletes it), which `symlink_metadata` agrees with.
    std::fs::symlink_metadata(&resolved)?;
    if resolved.parent().is_none() {
        return Ok(Err(Refusal::FilesystemRoot));
    }
    if resolved.file_name().is_none() {
        return Ok(Err(Refusal::NoFileName));
    }
    let home = canonical_lenient(agent_home)?;
    let trash = canonical_lenient(&trash_root(agent_home))?;
    if resolved.starts_with(&trash) {
        return Ok(Err(Refusal::InsideTrash));
    }
    if home.starts_with(&resolved) {
        return Ok(Err(Refusal::AgentHome));
    }
    Ok(Ok(resolved))
}

/// Permanently delete `path`, which must sit strictly below `trash_root`.
///
/// For `trash empty|now`: a link is removed as a link (never followed out of
/// the trash), a real directory is removed recursively, and the root itself
/// is never deleted.
pub fn remove_within(trash_root: &Path, path: &Path) -> io::Result<()> {
    let root = trash_root.canonicalize()?;
    let resolved = resolve_no_follow(path)?;
    if resolved == root || !resolved.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("not inside the trash root: {}", path.display()),
        ));
    }
    remove_any(&resolved)
}
