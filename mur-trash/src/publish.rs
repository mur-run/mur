//! Atomic no-replace rename: the only way a move may publish its object.
//!
//! A "does the destination exist?" check followed by `fs::rename` leaves a
//! window in which a newly created destination is silently replaced (review
//! #2168 #2). Every rename the mover does goes through [`rename_noreplace`],
//! which fails with `AlreadyExists` instead of replacing anything.

use std::fs;
use std::io;
use std::path::Path;

/// Rename `from` to `to`, failing with `AlreadyExists` if `to` exists — file,
/// directory, or dangling link — at the moment of the rename.
///
/// Uses the kernel's no-replace rename where there is one (Linux
/// `renameat2(RENAME_NOREPLACE)`, macOS `renamex_np(RENAME_EXCL)`), and
/// [`claim_then_rename`] where the platform or filesystem lacks it.
/// Like `fs::rename`, crossing filesystems fails with `CrossesDevices`.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    match native(from, to) {
        Some(Err(e)) if unsupported(&e) => claim_then_rename(from, to),
        Some(r) => r,
        None => claim_then_rename(from, to),
    }
}

/// The flag is not understood by this kernel or filesystem.
#[cfg(unix)]
fn unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP)
    )
}

#[cfg(not(unix))]
fn unsupported(_: &io::Error) -> bool {
    false
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn cstr(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn native(from: &Path, to: &Path) -> Option<io::Result<()>> {
    let (f, t) = match (cstr(from), cstr(to)) {
        (Ok(f), Ok(t)) => (f, t),
        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
    };
    // The raw syscall, not libc's wrapper: musl and older glibc lack one.
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    Some(if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn native(from: &Path, to: &Path) -> Option<io::Result<()>> {
    let (f, t) = match (cstr(from), cstr(to)) {
        (Ok(f), Ok(t)) => (f, t),
        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
    };
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    let rc = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    Some(if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    })
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(crate) fn native(_: &Path, _: &Path) -> Option<io::Result<()>> {
    None
}

/// Portable no-replace rename for when the kernel has none.
///
/// Non-directories: `hard_link` claims `to` atomically (it never replaces),
/// then `from` is unlinked. Directories, on Unix: `create_dir(to)` claims the
/// name, then the rename may only replace that empty directory — if anyone
/// has put something in it or swapped it for a file, the rename fails rather
/// than destroy it.
///
/// Where no claim is possible — Windows directories, filesystems without hard
/// links — it falls back to check-then-rename, which is not atomic.
pub(crate) fn claim_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        return claim_dir_then_rename(from, to);
    }
    match fs::hard_link(from, to) {
        Ok(()) => {}
        // A real answer about `to` or the filesystems: report it.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::AlreadyExists
                    | io::ErrorKind::CrossesDevices
                    | io::ErrorKind::NotFound
            ) =>
        {
            return Err(e);
        }
        // No hard links here (FAT, some network mounts): best effort.
        Err(_) => return check_then_rename(from, to),
    }
    if let Err(e) = fs::remove_file(from) {
        let _ = fs::remove_file(to); // our own link, same inode as `from`
        return Err(e);
    }
    Ok(())
}

/// Last resort where no claim is possible: not atomic, but never replaces
/// anything that existed before the call.
fn check_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", to.display()),
        ));
    }
    fs::rename(from, to)
}

#[cfg(unix)]
fn claim_dir_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    fs::rename(from, to).inspect_err(|_| {
        let _ = fs::remove_dir(to); // only succeeds while it is still our empty claim
    })
}

#[cfg(not(unix))]
fn claim_dir_then_rename(from: &Path, to: &Path) -> io::Result<()> {
    check_then_rename(from, to)
}
