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

/// Whether the user has agreed to proactive interjections this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    #[default]
    Unasked,
    Granted,
    Declined,
}

/// Persisted proactive-watch session state. Written by the MCP `watch_*` tools
/// (via `mur-core`) and read by the runtime `WatchScheduler`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WatchSession {
    pub active: bool,
    pub muted: bool,
    pub last_interjection_ms: i64,
    pub last_scene_phash: u64,
    pub consent: Consent,
}

/// Path to the persisted watch session.
pub fn watch_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("watch.json")
}

/// Load the watch session, or a default (all-off) session if absent/unparseable.
pub fn load_watch(mur_home: &Path) -> WatchSession {
    std::fs::read_to_string(watch_path(mur_home))
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_default()
}

/// Persist the watch session atomically (temp + rename).
pub fn save_watch(mur_home: &Path, s: &WatchSession) -> std::io::Result<()> {
    let path = watch_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(s).expect("serialize WatchSession");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn absent_session_is_default_off() {
        let home = TempDir::new().unwrap();
        let s = load_watch(home.path());
        assert!(!s.active);
        assert_eq!(s.consent, Consent::Unasked);
    }

    #[test]
    fn session_roundtrips() {
        let home = TempDir::new().unwrap();
        let s = WatchSession {
            active: true,
            muted: false,
            last_interjection_ms: 123,
            last_scene_phash: 0xABCD,
            consent: Consent::Granted,
        };
        save_watch(home.path(), &s).unwrap();
        assert_eq!(load_watch(home.path()), s);
    }
}
