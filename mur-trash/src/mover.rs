//! Move a path into the trash, across filesystems if needed (D4).
//!
//! Shared with #2158 (restore), which moves the other way with the same code.

use std::fs;
use std::io;
use std::path::Path;

use crate::layout::partial_path;

/// How a move that did not error ended.
#[derive(Debug)]
pub enum MoveOutcome {
    /// `dst` holds the object and `src` is gone.
    Moved,
    /// Cross-device: `dst` holds a full copy, but `src` could not be removed.
    /// Not success and not plain failure — the caller reports it distinctly.
    CopiedSourceKept(io::Error),
}

/// Move `src` to `dst`. `dst` must not exist; its parent is created.
///
/// `rename` first; on `EXDEV`, copy to `<dst>.partial` (links copied as links,
/// mode and mtime kept), rename that to `dst`, then remove `src`.
pub fn move_across(src: &Path, dst: &Path) -> io::Result<MoveOutcome> {
    move_with(src, dst, |a, b| fs::rename(a, b))
}

/// [`move_across`] with an injectable first `rename`, so tests can force EXDEV.
pub(crate) fn move_with(
    src: &Path,
    dst: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<MoveOutcome> {
    // Fail before touching anything if the source is gone.
    fs::symlink_metadata(src)?;
    if fs::symlink_metadata(dst).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("trash destination exists: {}", dst.display()),
        ));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    match rename(src, dst) {
        Ok(()) => return Ok(MoveOutcome::Moved),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {}
        Err(e) => return Err(e),
    }

    let partial = partial_path(dst);
    if let Err(e) = copy_tree(src, &partial) {
        let _ = remove_any(&partial);
        return Err(e);
    }
    fs::rename(&partial, dst)?;
    match remove_any(src) {
        Ok(()) => Ok(MoveOutcome::Moved),
        Err(e) => Ok(MoveOutcome::CopiedSourceKept(e)),
    }
}

/// Remove a file, link, or whole directory without following links.
pub(crate) fn remove_any(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path) // does not follow links inside
    } else {
        fs::remove_file(path)
    }
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        copy_link(src, dst)
    } else if ft.is_dir() {
        fs::create_dir(dst)?;
        for child in fs::read_dir(src)? {
            let child = child?;
            copy_tree(&child.path(), &dst.join(child.file_name()))?;
        }
        // Mode last, so a read-only source dir does not block its own copy.
        fs::set_permissions(dst, meta.permissions())?;
        set_mtime(dst, &meta);
        Ok(())
    } else if ft.is_file() {
        fs::copy(src, dst)?; // copies permission bits
        set_mtime(dst, &meta);
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot copy special file across devices: {}", src.display()),
        ))
    }
}

#[cfg(unix)]
fn copy_link(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(src)?, dst)
}

#[cfg(not(unix))]
fn copy_link(src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("cannot copy symlink across devices: {}", src.display()),
    ))
}

/// Best effort: a lost mtime is not worth failing a delete over.
fn set_mtime(path: &Path, meta: &fs::Metadata) {
    if let Ok(mtime) = meta.modified()
        && let Ok(f) = fs::File::open(path)
    {
        let _ = f.set_modified(mtime);
    }
}
