//! Move a path into the trash, across filesystems if needed (D4).
//!
//! Shared with #2158 (restore), which moves the other way with the same code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::layout::partial_path;

/// Name of the per-move directory under a staging root.
fn stage_dir(staging_root: &Path) -> PathBuf {
    staging_root.join(uuid::Uuid::new_v4().to_string())
}

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

/// [`move_across`] with an injectable first `rename`, so callers' tests can
/// force the cross-device path (macOS has no tmpfs to provoke a real EXDEV).
pub fn move_with(
    src: &Path,
    dst: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<MoveOutcome> {
    move_impl(src, dst, Staging::BesideDst, &|a, b| {
        if a == src {
            rename(a, b)
        } else {
            fs::rename(a, b)
        }
    })
}

/// Move into the trash: like [`move_across`], but a cross-device copy is
/// staged in `<staging_root>/<uuid>/` instead of beside `dst`, so a crash
/// never leaves anything inside an entry directory. `staging_root` must be on
/// `dst`'s filesystem (it is: both live under the trash root).
pub fn move_staged(src: &Path, dst: &Path, staging_root: &Path) -> io::Result<MoveOutcome> {
    move_with_staging(src, dst, staging_root, |a, b| fs::rename(a, b))
}

/// [`move_staged`] with an injectable `rename`, used for every rename.
pub(crate) fn move_with_staging(
    src: &Path,
    dst: &Path,
    staging_root: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<MoveOutcome> {
    move_impl(src, dst, Staging::Under(staging_root), &rename)
}

enum Staging<'a> {
    /// `<dst>.partial` — for moves out of the trash (restore).
    BesideDst,
    /// `<root>/<uuid>/<name>` — for moves into the trash.
    Under(&'a Path),
}

fn move_impl(
    src: &Path,
    dst: &Path,
    staging: Staging<'_>,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<MoveOutcome> {
    // Fail before touching anything if the source is gone.
    fs::symlink_metadata(src)?;
    if fs::symlink_metadata(dst).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", dst.display()),
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

    // `cleanup` is what to remove if the copy does not make it to `dst`.
    let (partial, cleanup) = match staging {
        Staging::BesideDst => {
            let p = partial_path(dst);
            (p.clone(), p)
        }
        Staging::Under(root) => {
            let dir = stage_dir(root);
            fs::create_dir_all(&dir)?;
            let name = dst.file_name().unwrap_or(std::ffi::OsStr::new("x"));
            (dir.join(name), dir)
        }
    };
    let staged = copy_tree(src, &partial).and_then(|()| rename(&partial, dst));
    if let Err(e) = staged {
        let _ = remove_any(&cleanup);
        return Err(e);
    }
    if !matches!(staging, Staging::BesideDst) {
        let _ = fs::remove_dir(&cleanup); // now empty
    }
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
