//! Persistent fetch cursor for the sync protocol.
//!
//! The cursor tracks the last-seen signal id so that subsequent
//! `GET /v1/signals/pending?since={cursor}` requests don't redeliver
//! already-applied signals.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Pagination state persisted to disk between `mur fetch` invocations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchCursor {
    /// Server-side opaque cursor (typically the last signal id accepted).
    #[serde(default)]
    pub last_signal_id: Option<String>,

    /// Wall-clock of the last successful fetch (for diagnostics + `mur sync status`).
    #[serde(default)]
    pub last_fetched_at: Option<DateTime<Utc>>,
}

/// Atomic on-disk cursor store.
pub struct CursorStore {
    path: PathBuf,
}

impl CursorStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Default location at `$HOME/.mur/sync/cursor.json`.
    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME directory"))?;
        let p = home.join(".mur/sync/cursor.json");
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self::new(p))
    }

    /// Load the cursor, returning a default (empty) cursor if the file is absent.
    pub fn load(&self) -> Result<FetchCursor> {
        if !self.path.exists() {
            return Ok(FetchCursor::default());
        }
        let s = std::fs::read_to_string(&self.path)
            .with_context(|| format!("read cursor {}", self.path.display()))?;
        serde_json::from_str(&s)
            .with_context(|| format!("parse cursor {}", self.path.display()))
    }

    /// Save the cursor atomically (temp-then-rename).
    pub fn save(&self, c: &FetchCursor) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        let payload = serde_json::to_string_pretty(c)
            .with_context(|| "serialize FetchCursor")?;
        std::fs::write(&tmp, payload)
            .with_context(|| format!("write tmp cursor {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename cursor to {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_returns_default() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        let c = cs.load().unwrap();
        assert!(c.last_signal_id.is_none());
        assert!(c.last_fetched_at.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        let now = Utc::now();
        let c = FetchCursor {
            last_signal_id: Some("signal-abc".into()),
            last_fetched_at: Some(now),
        };
        cs.save(&c).unwrap();
        let back = cs.load().unwrap();
        assert_eq!(back.last_signal_id, c.last_signal_id);
        assert_eq!(
            back.last_fetched_at.unwrap().timestamp_millis(),
            now.timestamp_millis()
        );
    }

    #[test]
    fn save_creates_parent_dir() {
        let tmp = tempdir().unwrap();
        let deep = tmp.path().join("a/b/c/cursor.json");
        let cs = CursorStore::new(&deep);
        cs.save(&FetchCursor::default()).unwrap();
        assert!(deep.exists());
    }

    #[test]
    fn save_is_atomic_no_tmp_leftover() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        cs.save(&FetchCursor {
            last_signal_id: Some("x".into()),
            last_fetched_at: None,
        })
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "expected no .tmp files after successful save");
    }

    #[test]
    fn load_rejects_malformed_json() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("cursor.json");
        std::fs::write(&path, "not valid json").unwrap();
        let cs = CursorStore::new(&path);
        let err = cs.load().unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("parse"), "expected parse error, got: {msg}");
    }

    #[test]
    fn overwrite_existing_cursor() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        cs.save(&FetchCursor {
            last_signal_id: Some("first".into()),
            last_fetched_at: None,
        })
        .unwrap();
        cs.save(&FetchCursor {
            last_signal_id: Some("second".into()),
            last_fetched_at: None,
        })
        .unwrap();
        let back = cs.load().unwrap();
        assert_eq!(back.last_signal_id.as_deref(), Some("second"));
    }
}
