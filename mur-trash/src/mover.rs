//! Move a path into the trash, across filesystems if needed (D4).
//!
//! Shared with #2158 (restore), which moves the other way with the same code.
//! Nothing a move did not create is ever written or removed: every rename is
//! no-replace ([`rename_noreplace`]) and every staging directory is created
//! exclusively under a fresh name (review #2168).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::publish::rename_noreplace;

/// Prefix of the hidden per-move staging directory a cross-device restore
/// creates beside its destination: `.<name>.mur-restore-<uuid>`.
pub const RESTORE_STAGING_PREFIX: &str = ".mur-restore-";

/// How a move that did not error ended.
#[derive(Debug)]
pub enum MoveOutcome {
    /// `dst` holds the object and `src` is gone.
    Moved,
    /// Cross-device: `dst` holds a full copy, but `src` could not be removed.
    /// Not success and not plain failure — the caller reports it distinctly.
    CopiedSourceKept(io::Error),
}

/// Move `src` to `dst`, never replacing anything at `dst`; its parent is
/// created.
///
/// No-replace `rename` first; on `EXDEV`, copy into a freshly created
/// directory beside `dst` (links copied as links, mode and mtime kept),
/// publish it to `dst` with a no-replace rename, then remove `src`.
pub fn move_across(src: &Path, dst: &Path) -> io::Result<MoveOutcome> {
    move_with(src, dst, rename_noreplace)
}

/// [`move_across`] with an injectable first `rename` (the one from `src`), so
/// callers' tests can force the cross-device path (macOS has no tmpfs to
/// provoke a real EXDEV). Later renames are always [`rename_noreplace`].
pub fn move_with(
    src: &Path,
    dst: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<MoveOutcome> {
    move_impl(src, dst, Staging::BesideDst, &|a, b| {
        if a == src {
            rename(a, b)
        } else {
            rename_noreplace(a, b)
        }
    })
}

/// Move into the trash: like [`move_across`], but a cross-device copy is
/// staged in `<staging_root>/<uuid>/` instead of beside `dst`, so a crash
/// never leaves anything inside an entry directory. `staging_root` must be on
/// `dst`'s filesystem (it is: both live under the trash root).
pub fn move_staged(src: &Path, dst: &Path, staging_root: &Path) -> io::Result<MoveOutcome> {
    move_with_staging(src, dst, staging_root, rename_noreplace)
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
    /// `<dst parent>/.<name>.mur-restore-<uuid>/` — for moves out of the
    /// trash (restore): the only directory known to be on `dst`'s filesystem.
    BesideDst,
    /// `<root>/<uuid>/` — for moves into the trash.
    Under(&'a Path),
}

/// Create a staging directory that did not exist before, so cleanup can never
/// touch anything this move did not make. A name clash is retried, not reused.
fn create_stage_dir(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    const ATTEMPTS: usize = 4;
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let dir = parent.join(format!("{prefix}{}", uuid::Uuid::new_v4()));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no staging directory")))
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

    // `stage` is created by this move and is the only thing cleanup removes.
    let name = dst.file_name().unwrap_or(std::ffi::OsStr::new("x"));
    let stage = match staging {
        Staging::BesideDst => {
            let parent = dst.parent().unwrap_or(Path::new("."));
            let prefix = format!(".{}{RESTORE_STAGING_PREFIX}", name.to_string_lossy());
            create_stage_dir(parent, &prefix)?
        }
        Staging::Under(root) => {
            fs::create_dir_all(root)?;
            create_stage_dir(root, "")?
        }
    };
    let partial = stage.join(name);
    let staged = copy_tree(src, &partial).and_then(|()| rename(&partial, dst));
    if let Err(e) = staged {
        let _ = remove_any(&stage);
        return Err(e);
    }
    let _ = fs::remove_dir(&stage); // now empty
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
