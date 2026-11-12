//! The requests directory, pinned once and then used only through that handle.
//!
//! The directory sits in a tree the agent can write. Checking the path and
//! then reading or deleting by path again leaves a window in which the agent
//! swaps the directory for a symlink, and `O_NOFOLLOW` on the final file only
//! guards the last component, not its parent. So on Unix the directory is
//! opened once with `O_DIRECTORY | O_NOFOLLOW` and every listing, open and
//! unlink after that is relative to the descriptor: a later swap of the path
//! changes nothing we touch.
//!
//! Entries are opened `O_NONBLOCK`: opening a FIFO with no writer would
//! otherwise block before its type could be checked, and a planted FIFO would
//! hang every client that lists requests.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
pub(crate) struct PinnedDir {
    fd: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl PinnedDir {
    /// Open `dir` itself without following a symlink at it.
    pub(crate) fn open(dir: &Path) -> io::Result<Self> {
        use rustix::fs::{CWD, Mode, OFlags, openat};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = openat(CWD, dir, flags, Mode::empty())?;
        Ok(Self { fd })
    }

    /// Entry names that are valid UTF-8; anything else cannot be an id.
    pub(crate) fn names(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in rustix::fs::Dir::read_from(&self.fd)? {
            let entry = entry?;
            if let Ok(name) = entry.file_name().to_str() {
                out.push(name.to_owned());
            }
        }
        Ok(out)
    }

    /// The entry `name` if it is a regular file; `None` for anything else
    /// (a link, a FIFO, a device, a directory). Never blocks on a FIFO.
    pub(crate) fn open_regular(&self, name: &str) -> io::Result<Option<File>> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = openat(&self.fd, name, flags, Mode::empty())?;
        if FileType::from_raw_mode(fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Ok(None);
        }
        Ok(Some(File::from(fd)))
    }

    /// Unlink the entry `name` itself, inside the pinned directory.
    pub(crate) fn unlink(&self, name: &str) -> io::Result<()> {
        rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())?;
        Ok(())
    }
}

/// No `openat` family: check the path, then act on it. The window is the
/// agent's own tree, as `mur_common::paths::open_no_follow` documents.
#[cfg(not(unix))]
pub(crate) struct PinnedDir {
    path: std::path::PathBuf,
}

#[cfg(not(unix))]
impl PinnedDir {
    pub(crate) fn open(dir: &Path) -> io::Result<Self> {
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        Ok(Self {
            path: dir.to_path_buf(),
        })
    }

    pub(crate) fn names(&self) -> io::Result<Vec<String>> {
        Ok(std::fs::read_dir(&self.path)?
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect())
    }

    pub(crate) fn open_regular(&self, name: &str) -> io::Result<Option<File>> {
        let f = mur_common::paths::open_no_follow(&self.path.join(name))?;
        Ok(f.metadata()?.is_file().then_some(f))
    }

    pub(crate) fn unlink(&self, name: &str) -> io::Result<()> {
        std::fs::remove_file(self.path.join(name))
    }
}
