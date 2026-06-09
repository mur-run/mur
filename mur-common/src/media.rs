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

/// Load the persisted VLC runtime (`vlc.json`), or `None` if absent/unparseable.
/// Used by the runtime supervisor to allowlist VLC's HTTP port in the kernel
/// sandbox, and anywhere else that needs the current VLC connection details.
pub fn load_runtime(mur_home: &Path) -> Option<VlcRuntime> {
    let raw = std::fs::read_to_string(runtime_path(mur_home)).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn runtime_path_under_runtime_dir() {
        let p = runtime_path(Path::new("/tmp/h"));
        assert!(p.ends_with("runtime/vlc.json"));
    }

    #[test]
    fn load_runtime_absent_is_none() {
        let home = TempDir::new().unwrap();
        assert!(load_runtime(home.path()).is_none());
    }

    #[test]
    fn load_runtime_roundtrips_port() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("runtime");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            runtime_path(home.path()),
            r#"{"port":61886,"password":"pw","snapshot_dir":"/tmp/s"}"#,
        )
        .unwrap();
        assert_eq!(load_runtime(home.path()).unwrap().port, 61886);
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
