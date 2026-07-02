//! `.muragent` reader — extract and inspect a signed agent package.

use crate::muragent::MuragentError;
use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use tar::Archive;

/// Resource bounds for an untrusted `.muragent`. A `.muragent` is a tar.gz from
/// an untrusted source (a friend, a download), so the reader must not let a tiny
/// archive decompress into unbounded memory (a gzip/tar bomb). These caps are
/// generous for a real agent bundle (manifest + a few icons/skills) but stop a
/// malicious package from OOM-ing the host on import.
const MAX_ENTRIES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB per file
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB decompressed total

#[derive(Debug)]
pub struct MuragentArchive {
    /// All files in the tarball keyed by path → raw bytes.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl MuragentArchive {
    /// Read and extract all files from a `.muragent` tar.gz.
    pub fn read(path: &Path) -> Result<Self, MuragentError> {
        Self::read_with_limits(path, MAX_ENTRIES, MAX_FILE_BYTES, MAX_TOTAL_BYTES)
    }

    /// Implementation of [`read`](Self::read) with explicit resource caps, so
    /// the bomb defenses can be exercised with small limits in tests.
    fn read_with_limits(
        path: &Path,
        max_entries: usize,
        max_file_bytes: u64,
        max_total_bytes: u64,
    ) -> Result<Self, MuragentError> {
        let file = std::fs::File::open(path).map_err(MuragentError::Io)?;
        let gz = GzDecoder::new(file);
        let mut archive = Archive::new(gz);
        let mut files = BTreeMap::new();
        let mut entry_count = 0usize;
        let mut total_bytes = 0u64;

        for entry in archive
            .entries()
            .map_err(|e| MuragentError::Other(format!("tar entries: {e}")))?
        {
            entry_count += 1;
            if entry_count > max_entries {
                return Err(MuragentError::Other(format!(
                    "too many entries in .muragent (>{max_entries})"
                )));
            }
            let mut entry = entry.map_err(|e| MuragentError::Other(format!("tar entry: {e}")))?;

            let entry_path = entry
                .path()
                .map_err(|e| MuragentError::Other(format!("entry path: {e}")))?
                .to_str()
                .ok_or_else(|| MuragentError::Other("non-UTF-8 path in tarball".into()))?
                .to_string();

            let entry_type = entry.header().entry_type();
            if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
                return Err(MuragentError::ExecutableContent(format!(
                    "symlinks not allowed in .muragent: {entry_path}"
                )));
            }

            if entry_type != tar::EntryType::Regular
                && entry_type != tar::EntryType::Directory
                && entry_type != tar::EntryType::GNULongName
                && entry_type != tar::EntryType::GNULongLink
            {
                return Err(MuragentError::ExecutableContent(format!(
                    "tar entry type {:?} not allowed: {entry_path}",
                    entry_type
                )));
            }

            // Skip directories — we don't need them in the map
            if entry_type == tar::EntryType::Directory {
                continue;
            }

            crate::muragent::jcs_canonical::validate_tarball_path(&entry_path)
                .map_err(|e| MuragentError::Other(e.to_string()))?;

            // Check mode bits — regular files must not be executable
            let mode = entry.header().mode().unwrap_or(0o644);
            crate::muragent::executable_ban::check_mode_bits(mode, false)
                .map_err(MuragentError::ExecutableContent)?;

            // Read with a per-file cap (read one byte past the limit to detect
            // overflow), then enforce the running decompressed-total cap. This
            // is what actually stops a gzip/tar bomb — the header size field is
            // attacker-controlled and cannot be trusted.
            let mut data = Vec::new();
            entry
                .by_ref()
                .take(max_file_bytes + 1)
                .read_to_end(&mut data)
                .map_err(MuragentError::Io)?;
            if data.len() as u64 > max_file_bytes {
                return Err(MuragentError::Other(format!(
                    "file exceeds {max_file_bytes} bytes in .muragent: {entry_path}"
                )));
            }
            total_bytes += data.len() as u64;
            if total_bytes > max_total_bytes {
                return Err(MuragentError::Other(format!(
                    "decompressed .muragent exceeds {max_total_bytes} bytes total"
                )));
            }

            files.insert(entry_path, data);
        }

        Ok(Self { files })
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|v| v.as_slice())
    }

    pub fn get_str(&self, path: &str) -> Result<&str, MuragentError> {
        let bytes = self
            .get(path)
            .ok_or_else(|| MuragentError::Other(format!("file not found: {path}")))?;
        std::str::from_utf8(bytes)
            .map_err(|e| MuragentError::Other(format!("{path} is not valid UTF-8: {e}")))
    }

    pub fn files_as_vec(&self) -> Vec<(String, Vec<u8>)> {
        self.files
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build an in-memory `.muragent`-shaped tar.gz from (path, bytes) pairs.
    fn make_targz(files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        let gz = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.muragent");
        // Keep the tempdir alive by leaking it — fine for a unit test.
        std::mem::forget(dir);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&gz).unwrap();
        path
    }

    #[test]
    fn within_limits_reads_ok() {
        let p = make_targz(&[("a.txt", b"hello"), ("b.txt", b"world")]);
        let arc = MuragentArchive::read_with_limits(&p, 10, 1024, 4096).unwrap();
        assert_eq!(arc.files.len(), 2);
    }

    #[test]
    fn rejects_oversized_single_file() {
        let p = make_targz(&[("big.bin", &[0u8; 200])]);
        let err = MuragentArchive::read_with_limits(&p, 10, 100, 1_000_000).unwrap_err();
        assert!(format!("{err}").contains("exceeds"), "got: {err}");
    }

    #[test]
    fn rejects_oversized_total() {
        let p = make_targz(&[("a.bin", &[0u8; 100]), ("b.bin", &[0u8; 100])]);
        let err = MuragentArchive::read_with_limits(&p, 10, 1024, 150).unwrap_err();
        assert!(format!("{err}").contains("total"), "got: {err}");
    }

    #[test]
    fn rejects_too_many_entries() {
        let p = make_targz(&[("a", b"1"), ("b", b"2"), ("c", b"3")]);
        let err = MuragentArchive::read_with_limits(&p, 2, 1024, 4096).unwrap_err();
        assert!(format!("{err}").contains("too many entries"), "got: {err}");
    }
}
