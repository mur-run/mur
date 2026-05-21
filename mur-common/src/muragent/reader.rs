//! `.muragent` reader — extract and inspect a signed agent package.

use crate::muragent::MuragentError;
use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use tar::Archive;

pub struct MuragentArchive {
    /// All files in the tarball keyed by path → raw bytes.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl MuragentArchive {
    /// Read and extract all files from a `.muragent` tar.gz.
    pub fn read(path: &Path) -> Result<Self, MuragentError> {
        let file = std::fs::File::open(path).map_err(MuragentError::Io)?;
        let gz = GzDecoder::new(file);
        let mut archive = Archive::new(gz);
        let mut files = BTreeMap::new();

        for entry in archive
            .entries()
            .map_err(|e| MuragentError::Other(format!("tar entries: {e}")))?
        {
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

            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(MuragentError::Io)?;

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
