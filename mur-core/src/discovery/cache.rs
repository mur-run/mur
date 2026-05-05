//! On-disk cache for discovery results, at `~/.mur/cache/discovery.json`.
//! TTL 24h. Schema versioned. Best-effort: corrupt JSON or schema mismatch
//! is logged and treated as empty (forces re-discovery, never errors).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{Backend, DiscoveredModel};

pub const CACHE_SCHEMA_VERSION: u32 = 1;
pub const CACHE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryCache {
    pub schema_version: u32,
    pub entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub endpoint: String,
    pub backend: Backend,
    pub captured_at: DateTime<Utc>,
    pub models: Vec<DiscoveredModel>,
}

impl DiscoveryCache {
    pub fn empty() -> Self {
        Self { schema_version: CACHE_SCHEMA_VERSION, entries: Vec::new() }
    }

    /// Default cache path under the active mur root.
    pub fn default_path() -> PathBuf {
        crate::paths::mur_root(None).join("cache").join("discovery.json")
    }

    /// Load cache from disk. Returns `empty()` on missing file, corrupt
    /// JSON, or schema mismatch. Never errors — discovery just re-runs.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::empty();
        };
        match serde_json::from_slice::<DiscoveryCache>(&bytes) {
            Ok(c) if c.schema_version == CACHE_SCHEMA_VERSION => c,
            Ok(c) => {
                tracing::warn!(
                    found = c.schema_version,
                    expected = CACHE_SCHEMA_VERSION,
                    "discovery cache schema mismatch; ignoring"
                );
                Self::empty()
            }
            Err(e) => {
                tracing::warn!(?e, "discovery cache corrupt; ignoring");
                Self::empty()
            }
        }
    }

    /// Atomic write via temp + rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Look up a fresh entry for (endpoint, backend). Returns None if
    /// missing or older than `CACHE_TTL_HOURS`.
    pub fn fresh_entry(&self, endpoint: &str, backend: Backend) -> Option<&CacheEntry> {
        let cutoff = Utc::now() - Duration::hours(CACHE_TTL_HOURS);
        self.entries
            .iter()
            .find(|e| e.endpoint == endpoint && e.backend == backend && e.captured_at >= cutoff)
    }

    /// Insert or replace the entry for (endpoint, backend).
    pub fn upsert(&mut self, endpoint: String, backend: Backend, models: Vec<DiscoveredModel>) {
        self.entries.retain(|e| !(e.endpoint == endpoint && e.backend == backend));
        self.entries.push(CacheEntry {
            endpoint,
            backend,
            captured_at: Utc::now(),
            models,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveredModel, ModelKind};
    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    fn sample_model() -> DiscoveredModel {
        DiscoveredModel {
            id: "qwen3-embedding:0.6b".into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(1024),
            family: Some("bert".into()),
            size_bytes: None,
            probed_at: None,
        }
    }

    #[test]
    fn round_trip_save_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("discovery.json");

        let mut c = DiscoveryCache::empty();
        c.upsert(
            "http://localhost:11434".into(),
            Backend::Ollama,
            vec![sample_model()],
        );
        c.save(&path).unwrap();

        let loaded = DiscoveryCache::load(&path);
        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].models.len(), 1);
        assert_eq!(loaded.entries[0].models[0].id, "qwen3-embedding:0.6b");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let c = DiscoveryCache::load(&dir.path().join("nope.json"));
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn corrupt_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let c = DiscoveryCache::load(&path);
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn schema_mismatch_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("oldschema.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 999, "entries": []}"#,
        ).unwrap();
        let c = DiscoveryCache::load(&path);
        assert_eq!(c.entries.len(), 0);
    }

    #[test]
    fn fresh_entry_respects_ttl() {
        let mut c = DiscoveryCache::empty();
        c.upsert("ep".into(), Backend::Ollama, vec![sample_model()]);
        assert!(c.fresh_entry("ep", Backend::Ollama).is_some());

        // Manually age the entry past TTL
        c.entries[0].captured_at = Utc::now() - ChronoDuration::hours(CACHE_TTL_HOURS + 1);
        assert!(c.fresh_entry("ep", Backend::Ollama).is_none());
    }

    #[test]
    fn upsert_replaces_existing() {
        let mut c = DiscoveryCache::empty();
        c.upsert("ep".into(), Backend::Ollama, vec![]);
        c.upsert("ep".into(), Backend::Ollama, vec![sample_model()]);
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].models.len(), 1);
    }
}
