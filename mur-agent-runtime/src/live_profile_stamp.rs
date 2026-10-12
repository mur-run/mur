//! [`FileStamp`]: the cheap "did this file change?" check behind `LiveFile`.

use std::hash::{Hash, Hasher};
use std::path::Path;

/// Identity of one version of a file on disk: size + nanosecond mtime + a
/// third component that tells apart two same-length writes inside one mtime
/// tick. On Unix that is the inode — every MUR writer saves YAML by
/// temp-file-and-rename, which always changes it. Where no inode is exposed
/// (Windows), it is a hash of the bytes instead (#1812); the files watched
/// are small config YAML, so reading them per check is cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    mtime_nanos: u128,
    identity: u64,
}

impl FileStamp {
    /// `None` when the file is absent or unreadable — itself a state, since it
    /// is how a deletion shows up.
    pub fn of(path: &Path) -> Option<Self> {
        Self::with_identity(path, inode)
    }

    fn with_identity(path: &Path, inode: fn(&std::fs::Metadata) -> Option<u64>) -> Option<Self> {
        let md = std::fs::metadata(path).ok()?;
        let mtime_nanos = md
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let identity = match inode(&md) {
            Some(ino) => ino,
            None => content_hash(path)?,
        };
        Some(Self {
            len: md.len(),
            mtime_nanos,
            identity,
        })
    }
}

/// Process-local fingerprint of the bytes; stamps are never persisted, so the
/// hasher need not be stable across builds.
fn content_hash(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

#[cfg(unix)]
fn inode(md: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.ino())
}

#[cfg(not(unix))]
fn inode(_md: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn write_at(path: &Path, text: &str, mtime: SystemTime) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, path).unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(mtime).unwrap();
    }

    fn no_inode(_: &std::fs::Metadata) -> Option<u64> {
        None
    }

    /// #1812: without an inode, a same-length rewrite inside one mtime tick
    /// must still produce a different stamp.
    #[test]
    fn same_length_same_tick_differs_without_inode() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("global.yaml");
        let tick = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        write_at(&path, "deadline: 10m\n", tick);
        let before = FileStamp::with_identity(&path, no_inode).unwrap();
        write_at(&path, "deadline: 20m\n", tick);
        let after = FileStamp::with_identity(&path, no_inode).unwrap();
        assert_ne!(before, after);
    }

    /// Unchanged bytes keep the same stamp, so nothing reloads spuriously.
    #[test]
    fn unchanged_file_keeps_its_stamp_without_inode() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("global.yaml");
        write_at(
            &path,
            "deadline: 10m\n",
            UNIX_EPOCH + Duration::from_secs(1),
        );
        let a = FileStamp::with_identity(&path, no_inode);
        assert_eq!(a, FileStamp::with_identity(&path, no_inode));
        assert!(a.is_some());
    }

    #[test]
    fn missing_file_has_no_stamp() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(FileStamp::of(&d.path().join("absent.yaml")), None);
    }
}
