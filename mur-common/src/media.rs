//! Shared media runtime types (no business logic). Consumed by both `mur-core`
//! (VLC control, media tools) and `mur-agent-runtime` (WatchScheduler).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Per-session VLC HTTP connection details. Generated once and persisted so
/// repeated tool calls reach the same running VLC instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlcRuntime {
    pub port: u16,
    pub password: String,
    /// Directory VLC writes snapshots to (`--snapshot-path`).
    pub snapshot_dir: PathBuf,
}

/// Path to the persisted VLC runtime config.
pub fn runtime_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("vlc.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_path_under_runtime_dir() {
        let p = runtime_path(Path::new("/tmp/h"));
        assert!(p.ends_with("runtime/vlc.json"));
    }
}
