//! Atomic file replacement for the write-capable tools.
//!
//! `write_file` and `edit_file` go through [`replace_contents`] so a file is
//! never observable half-written: the turn track's `diff_files` compares
//! on-disk content, and a partial write it happened to read would be
//! reported as a change the turn did not make (spec §4.2). Temp file in the
//! target's own directory, then `rename` — the same shape `store/yaml.rs`
//! uses for YAML, kept separate because this one preserves the target's
//! permissions and is called from tools, not the store.

use std::io::Write;
use std::path::Path;

/// Write `bytes` to `target` atomically. Existing permission bits are kept;
/// a new file gets the platform default. The temp file is removed on any
/// failure, so a crash mid-write leaves the original untouched.
pub(crate) fn replace_contents(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let mode = std::fs::metadata(target).ok().map(|m| m.permissions());
    let mut tmp = tempfile::Builder::new()
        .prefix(".mur-write-")
        .tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    if let Some(perm) = mode {
        tmp.as_file().set_permissions(perm)?;
    }
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_new_and_overwrites_existing() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("a.txt");
        replace_contents(&p, b"one").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        replace_contents(&p, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("a.txt");
        replace_contents(&p, b"x").unwrap();
        let names: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_existing_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("run.sh");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        replace_contents(&p, b"#!/bin/sh\necho hi\n").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bit must survive the rename");
    }

    #[test]
    fn missing_parent_is_an_error_not_a_panic() {
        let td = tempfile::tempdir().unwrap();
        let err = replace_contents(&td.path().join("no/such/a.txt"), b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
