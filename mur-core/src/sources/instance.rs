//! Per-source config + sync state, persisted as `~/.mur/sources/<id>.yaml`.
//!
//! YAML is the source of truth, mirroring the pattern/workflow yaml stores.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use super::kind::SourceKind;

const MAX_ERRORS_TAIL: usize = 50;

/// Complete state for one connected source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInstance {
    pub id: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub kind: SourceKind,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    #[serde(default = "default_weight")]
    pub weight: f32,

    #[serde(default)]
    pub scope: BTreeMap<String, serde_yaml::Value>,

    #[serde(default)]
    pub sync: SyncState,

    #[serde(default)]
    pub stats: SourceStats,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyring_entry: Option<String>,
}

fn default_enabled() -> bool {
    true
}
fn default_weight() -> f32 {
    1.0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub errors_tail: Vec<SyncError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncError {
    pub at: DateTime<Utc>,
    pub doc: String,
    pub msg: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStats {
    #[serde(default)]
    pub doc_count: u64,
    #[serde(default)]
    pub chunk_count: u64,
    #[serde(default)]
    pub indexed_bytes: u64,
}

impl SyncState {
    /// Append an error, keeping the tail bounded.
    pub fn push_error(&mut self, err: SyncError) {
        self.errors_tail.push(err);
        let overflow = self.errors_tail.len().saturating_sub(MAX_ERRORS_TAIL);
        if overflow > 0 {
            self.errors_tail.drain(0..overflow);
        }
    }
}

/// Filesystem store: one yaml per source at `<root>/sources/<id>.yaml`.
pub struct SourceInstanceStore {
    root: PathBuf,
}

impl SourceInstanceStore {
    /// Default is `~/.mur/sources/`.
    pub fn default_store() -> Result<Self> {
        let root = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("sources");
        Ok(Self::new(root))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        // NOTE: ':' is allowed on macOS+Linux filesystems. Windows NTFS is not
        // a Phase 1 target; if we pivot to Windows, sanitize here.
        self.root.join(format!("{id}.yaml"))
    }

    pub fn save(&self, instance: &SourceInstance) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create dir {}", self.root.display()))?;
        let yaml = serde_yaml::to_string(instance)?;
        let target = self.path_for(&instance.id);
        let tmp = target.with_extension("yaml.tmp");
        fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &target)
            .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<SourceInstance> {
        let p = self.path_for(id);
        let content = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let inst: SourceInstance =
            serde_yaml::from_str(&content).with_context(|| format!("parse {}", p.display()))?;
        if inst.id != id {
            bail!(
                "file {} has id {} but we asked for {}",
                p.display(),
                inst.id,
                id
            );
        }
        Ok(inst)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let p = self.path_for(id);
        if p.exists() {
            fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<SourceInstance>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let content = fs::read_to_string(&p)?;
            match serde_yaml::from_str::<SourceInstance>(&content) {
                Ok(inst) => out.push(inst),
                Err(e) => {
                    tracing::warn!(file = %p.display(), error = %e, "skipping malformed source yaml");
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_instance(id: &str) -> SourceInstance {
        SourceInstance {
            id: id.into(),
            type_name: "obsidian".into(),
            kind: SourceKind::PullIndex,
            enabled: true,
            weight: 1.0,
            scope: BTreeMap::new(),
            sync: SyncState::default(),
            stats: SourceStats::default(),
            keyring_entry: None,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        let inst = sample_instance("obsidian-main");
        store.save(&inst).unwrap();
        let loaded = store.load("obsidian-main").unwrap();
        assert_eq!(loaded.id, "obsidian-main");
        assert_eq!(loaded.type_name, "obsidian");
        assert!(loaded.enabled);
    }

    #[test]
    fn list_returns_sorted_instances() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("b-second")).unwrap();
        store.save(&sample_instance("a-first")).unwrap();
        let items = store.list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a-first");
        assert_eq!(items[1].id, "b-second");
    }

    #[test]
    fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("obsidian-main")).unwrap();
        store.delete("obsidian-main").unwrap();
        assert!(store.load("obsidian-main").is_err());
    }

    #[test]
    fn delete_missing_is_ok() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.delete("never-existed").unwrap();
    }

    #[test]
    fn errors_tail_bounded_to_fifty() {
        let mut s = SyncState::default();
        for i in 0..60 {
            s.push_error(SyncError {
                at: Utc::now(),
                doc: format!("doc-{i}"),
                msg: "boom".into(),
            });
        }
        assert_eq!(s.errors_tail.len(), MAX_ERRORS_TAIL);
        assert_eq!(s.errors_tail[0].doc, "doc-10");
        assert_eq!(s.errors_tail.last().unwrap().doc, "doc-59");
    }

    #[test]
    fn load_rejects_id_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = SourceInstanceStore::new(tmp.path().to_path_buf());
        store.save(&sample_instance("real-id")).unwrap();
        fs::rename(
            tmp.path().join("real-id.yaml"),
            tmp.path().join("other-id.yaml"),
        )
        .unwrap();
        assert!(store.load("other-id").is_err());
    }
}
