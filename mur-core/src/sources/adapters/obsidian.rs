//! Obsidian vault adapter.
//!
//! Treats a local folder containing markdown files as a pull-index source.
//! Excludes `.obsidian/` (app state), `.trash/`, and any user-configured
//! folders via `SourceInstance.scope.exclude_folders`.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

use crate::sources::KnowledgeSource;
#[allow(unused_imports)]
use crate::sources::chunker::markdown as md;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
#[allow(unused_imports)]
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

const EXCLUDED_SEGMENTS: &[&str] = &[".obsidian", ".trash"];
#[allow(dead_code)]
const CHUNK_MAX_CHARS: usize = 6000;

/// Obsidian vault adapter.
pub struct ObsidianAdapter {
    id: String,
    vault_path: PathBuf,
    weight: f32,
    exclude_folders: Vec<String>,
}

impl ObsidianAdapter {
    /// Build from a `SourceInstance` (expects `type_name == "obsidian"` and the
    /// `scope.vault` value set to an absolute path).
    pub fn from_instance(instance: &SourceInstance) -> Result<Self> {
        if instance.type_name != "obsidian" {
            bail!(
                "expected type_name 'obsidian', got '{}'",
                instance.type_name
            );
        }
        let vault_val = instance
            .scope
            .get("vault")
            .context("source instance missing scope.vault")?;
        let vault_str: String = match vault_val {
            serde_yaml::Value::String(s) => s.clone(),
            _ => bail!("scope.vault must be a string"),
        };
        let vault_path = PathBuf::from(&vault_str);
        if !vault_path.is_dir() {
            bail!("vault path does not exist or is not a directory: {vault_str}");
        }
        if !vault_path.join(".obsidian").exists() {
            tracing::warn!(
                "vault {} has no .obsidian/ subdir — proceeding anyway",
                vault_path.display()
            );
        }

        let exclude_folders: Vec<String> = instance
            .scope
            .get("exclude_folders")
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            id: instance.id.clone(),
            vault_path,
            weight: instance.weight,
            exclude_folders,
        })
    }

    fn is_excluded(&self, rel: &Path) -> bool {
        for seg in rel.components() {
            if let Some(s) = seg.as_os_str().to_str() {
                if EXCLUDED_SEGMENTS.iter().any(|x| *x == s) {
                    return true;
                }
                if self.exclude_folders.iter().any(|e| e == s) {
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl KnowledgeSource for ObsidianAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> SourceKind {
        SourceKind::PullIndex
    }

    fn weight(&self) -> f32 {
        self.weight
    }

    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        let threshold: Option<DateTime<Utc>> = cursor.and_then(|c| {
            if c.is_empty() {
                None
            } else {
                DateTime::parse_from_rfc3339(&c.0).ok().map(|dt| dt.with_timezone(&Utc))
            }
        });

        let mut docs: Vec<DocRef> = Vec::new();
        let mut max_ts: Option<DateTime<Utc>> = None;

        for entry in walkdir::WalkDir::new(&self.vault_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let rel = path
                .strip_prefix(&self.vault_path)
                .ok()
                .map(|r| r.to_path_buf())
                .unwrap_or_else(|| path.to_path_buf());
            if self.is_excluded(&rel) {
                continue;
            }
            let meta = entry.metadata().context("stat vault file")?;
            let modified = meta.modified().context("no mtime on vault file")?;
            let updated_at: DateTime<Utc> = modified.into();

            if let Some(t) = threshold
                && updated_at <= t
            {
                continue;
            }
            if max_ts.map_or(true, |m| updated_at > m) {
                max_ts = Some(updated_at);
            }
            let external_id = rel
                .to_str()
                .context("vault path is not valid UTF-8")?
                .to_string();
            let title = rel
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            docs.push(DocRef {
                external_id,
                title,
                updated_at,
            });
        }

        let cursor_out = match max_ts {
            Some(t) => SyncCursor(t.to_rfc3339()),
            None => SyncCursor(threshold.map(|t| t.to_rfc3339()).unwrap_or_default()),
        };

        Ok((docs, cursor_out))
    }

    async fn fetch(&self, _doc_ref: &DocRef) -> Result<Document> {
        anyhow::bail!("ObsidianAdapter::fetch arrives in Task 9")
    }

    fn chunk(&self, _doc: &Document) -> Result<Vec<Chunk>> {
        anyhow::bail!("ObsidianAdapter::chunk arrives in Task 10")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        fs::write(tmp.path().join("note-a.md"), "# A\n\ncontent a").unwrap();
        fs::create_dir_all(tmp.path().join("folder")).unwrap();
        fs::write(tmp.path().join("folder").join("note-b.md"), "# B\n\ncontent b").unwrap();
        fs::create_dir_all(tmp.path().join(".trash")).unwrap();
        fs::write(tmp.path().join(".trash").join("deleted.md"), "# D").unwrap();
        tmp
    }

    fn make_instance(id: &str, vault: &Path) -> SourceInstance {
        let mut scope = BTreeMap::new();
        scope.insert(
            "vault".into(),
            serde_yaml::Value::String(vault.to_string_lossy().to_string()),
        );
        SourceInstance {
            id: id.into(),
            type_name: "obsidian".into(),
            kind: SourceKind::PullIndex,
            enabled: true,
            weight: 1.0,
            scope,
            sync: crate::sources::instance::SyncState::default(),
            stats: crate::sources::instance::SourceStats::default(),
            keyring_entry: None,
        }
    }

    #[tokio::test]
    async fn from_instance_validates_vault_exists() {
        let inst = {
            let mut i = make_instance("obsidian-x", Path::new("/nonexistent/path/xyz"));
            i.scope.insert(
                "vault".into(),
                serde_yaml::Value::String("/nonexistent/path/xyz".into()),
            );
            i
        };
        assert!(ObsidianAdapter::from_instance(&inst).is_err());
    }

    #[tokio::test]
    async fn list_documents_finds_md_files_excluding_hidden() {
        let tmp = make_vault();
        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _cursor) = adapter.list_documents(None).await.unwrap();
        let mut ids: Vec<String> = docs.iter().map(|d| d.external_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["folder/note-b.md".to_string(), "note-a.md".to_string()]
        );
    }

    #[tokio::test]
    async fn exclude_folder_is_honoured() {
        let tmp = make_vault();
        let mut inst = make_instance("obsidian-main", tmp.path());
        inst.scope.insert(
            "exclude_folders".into(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::String("folder".into())]),
        );
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let ids: Vec<String> = docs.iter().map(|d| d.external_id.clone()).collect();
        assert_eq!(ids, vec!["note-a.md".to_string()]);
    }

    #[tokio::test]
    async fn cursor_filters_older_files() {
        let tmp = make_vault();
        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let future = chrono::Utc::now() + chrono::Duration::days(365);
        let (docs, _) = adapter
            .list_documents(Some(SyncCursor(future.to_rfc3339())))
            .await
            .unwrap();
        assert!(docs.is_empty());
    }
}
