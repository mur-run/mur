//! First-run extraction — unpacks an embedded agent tar.gz to a stable
//! cache directory keyed by content digest. Subsequent runs short-circuit
//! when the on-disk `.extract_digest` marker matches.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tar::Archive;

#[derive(Debug, Clone)]
pub struct ExtractInfo {
    pub agent_home: PathBuf,
    pub digest: String,
    /// True when this call performed a fresh extraction; false when an
    /// existing `.extract_digest` marker matched and nothing was unpacked.
    pub fresh: bool,
}

/// Default cache base used by `extract_embedded`: $XDG_CACHE_HOME/murmur,
/// or ~/.cache/murmur as a fallback.
pub fn default_cache_base() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("murmur");
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".cache").join("murmur");
    }
    PathBuf::from("/tmp/murmur")
}

/// Extract `tar_gz` bytes into `<base_dir>/<digest12>/`. Returns the
/// computed digest, the resolved agent_home, and `fresh=true` only when
/// content was actually unpacked.
pub fn extract_embedded_to(tar_gz: &[u8], base_dir: &Path) -> Result<ExtractInfo> {
    let digest = sha256_hex(tar_gz);
    let short = &digest[..12.min(digest.len())];
    let agent_home = base_dir.join(short);
    let marker = agent_home.join(".extract_digest");

    if agent_home.exists()
        && let Ok(prev) = fs::read_to_string(&marker)
        && prev.trim() == digest
    {
        return Ok(ExtractInfo {
            agent_home,
            digest,
            fresh: false,
        });
    }

    fs::create_dir_all(&agent_home).with_context(|| format!("create {}", agent_home.display()))?;
    let gz = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(gz);
    archive
        .unpack(&agent_home)
        .with_context(|| format!("unpack into {}", agent_home.display()))?;

    fs::write(&marker, &digest).with_context(|| format!("write {}", marker.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&marker)?.permissions();
        perms.set_mode(0o600);
        let _ = fs::set_permissions(&marker, perms);
    }

    Ok(ExtractInfo {
        agent_home,
        digest,
        fresh: true,
    })
}

/// Convenience wrapper that uses `default_cache_base()`.
pub fn extract_embedded(tar_gz: &[u8]) -> Result<ExtractInfo> {
    extract_embedded_to(tar_gz, &default_cache_base())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}
