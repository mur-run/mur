//! macOS `sun_path` is 104 bytes; Linux is 108. When the canonical
//! agent socket path is too long, bind in /tmp and symlink back.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_SAFE_PATH_BYTES: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum SocketPathError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct BindResolution {
    pub bind_path: PathBuf,
    pub canonical_path: PathBuf,
    pub symlink_created: bool,
}

pub fn resolve_bind_target(
    canonical: &Path,
    uuid: &str,
) -> Result<BindResolution, SocketPathError> {
    let canonical_bytes = canonical.as_os_str().len();
    if canonical_bytes <= MAX_SAFE_PATH_BYTES {
        return Ok(BindResolution {
            bind_path: canonical.to_path_buf(),
            canonical_path: canonical.to_path_buf(),
            symlink_created: false,
        });
    }
    let short = PathBuf::from(format!(
        "/tmp/mur-{}.sock",
        uuid.chars().take(8).collect::<String>()
    ));
    if let Some(parent) = canonical.parent() {
        fs::create_dir_all(parent)?;
    }
    if canonical.exists() || canonical.symlink_metadata().is_ok() {
        fs::remove_file(canonical)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&short, canonical)?;
    #[cfg(not(unix))]
    fs::copy(&short, canonical).map(|_| ())?;
    Ok(BindResolution {
        bind_path: short,
        canonical_path: canonical.to_path_buf(),
        symlink_created: true,
    })
}
