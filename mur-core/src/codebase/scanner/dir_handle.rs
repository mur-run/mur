//! Directory handles the scanner walks through: every child is opened
//! relative to its parent's handle, never re-resolved by path.

use std::fs;
use std::path::Path;
#[cfg(not(unix))]
use std::path::PathBuf;

use super::EntryKind;

/// Largest file the scanner indexes, in bytes.
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Reads an opened file only if it is a regular file within the size cap.
/// The cap applies to what is read, not just what was stat'ed.
fn read_capped(file: fs::File) -> Option<Vec<u8>> {
    use std::io::Read;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

/// An open directory every child is resolved against (`openat`), so no
/// path component above it is ever re-resolved.
#[cfg(unix)]
pub(super) struct DirHandle(rustix::fd::OwnedFd);

#[cfg(unix)]
impl DirHandle {
    fn dir_flags() -> rustix::fs::OFlags {
        use rustix::fs::OFlags;
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC
    }

    /// The root itself is the caller's choice, so it may be reached via a link.
    pub(super) fn open_root(path: &Path) -> Option<Self> {
        rustix::fs::open(path, Self::dir_flags(), rustix::fs::Mode::empty())
            .ok()
            .map(Self)
    }

    pub(super) fn entries(&self) -> Vec<(std::ffi::OsString, EntryKind)> {
        use rustix::fs::{AtFlags, FileType};
        use std::os::unix::ffi::OsStrExt;
        let Ok(dir) = rustix::fs::Dir::read_from(&self.0) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            let name = std::ffi::OsStr::from_bytes(raw).to_os_string();
            let ft = match entry.file_type() {
                FileType::Unknown => {
                    match rustix::fs::statat(&self.0, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                    {
                        Ok(st) => FileType::from_raw_mode(st.st_mode as _),
                        Err(_) => continue,
                    }
                }
                ft => ft,
            };
            let kind = match ft {
                FileType::Directory => EntryKind::Dir,
                FileType::RegularFile => EntryKind::File,
                _ => EntryKind::Other,
            };
            out.push((name, kind));
        }
        out
    }

    /// Fails if `name` is no longer a real directory (e.g. now a symlink).
    pub(super) fn open_dir(&self, name: &std::ffi::OsStr) -> Option<Self> {
        use rustix::fs::OFlags;
        let flags = Self::dir_flags() | OFlags::NOFOLLOW;
        rustix::fs::openat(&self.0, name, flags, rustix::fs::Mode::empty())
            .ok()
            .map(Self)
    }

    /// `NOFOLLOW` refuses a swapped-in link; `NONBLOCK` keeps a swapped-in
    /// FIFO from hanging the open (it is then rejected as not a regular file).
    pub(super) fn read_file(&self, name: &std::ffi::OsStr) -> Option<Vec<u8>> {
        use rustix::fs::OFlags;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.0, name, flags, rustix::fs::Mode::empty()).ok()?;
        read_capped(fs::File::from(fd))
    }
}

/// Path-based fallback where `openat` is unavailable. Existing links are
/// still skipped (`DirEntry::file_type` reports the link itself), but a
/// directory swapped mid-walk is not pinned.
#[cfg(not(unix))]
pub(super) struct DirHandle(PathBuf);

#[cfg(not(unix))]
impl DirHandle {
    pub(super) fn open_root(path: &Path) -> Option<Self> {
        Some(Self(path.to_path_buf()))
    }

    pub(super) fn entries(&self) -> Vec<(std::ffi::OsString, EntryKind)> {
        let Ok(rd) = fs::read_dir(&self.0) else {
            return Vec::new();
        };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                let kind = if ft.is_dir() {
                    EntryKind::Dir
                } else if ft.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                };
                Some((e.file_name(), kind))
            })
            .collect()
    }

    pub(super) fn open_dir(&self, name: &std::ffi::OsStr) -> Option<Self> {
        let p = self.0.join(name);
        let meta = fs::symlink_metadata(&p).ok()?;
        meta.is_dir().then_some(Self(p))
    }

    pub(super) fn read_file(&self, name: &std::ffi::OsStr) -> Option<Vec<u8>> {
        let p = self.0.join(name);
        if !fs::symlink_metadata(&p).ok()?.is_file() {
            return None;
        }
        read_capped(fs::File::open(p).ok()?)
    }
}
