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
use crate::sources::chunker::markdown as md;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

const EXCLUDED_SEGMENTS: &[&str] = &[".obsidian", ".trash"];
const CHUNK_MAX_CHARS: usize = 6000;

/// Obsidian vault adapter.
pub struct ObsidianAdapter {
    id: String,
    vault_path: PathBuf,
    // Applied at search-time by the P1.3+ orchestrator via KnowledgeSource::weight().
    #[allow(dead_code)]
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
                if EXCLUDED_SEGMENTS.contains(&s) {
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
            if max_ts.is_none_or(|m| updated_at > m) {
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

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        let full_path = self.vault_path.join(&doc_ref.external_id);
        let raw = tokio::fs::read_to_string(&full_path)
            .await
            .with_context(|| format!("read vault file {}", full_path.display()))?;
        let (frontmatter, body_without_fm) = strip_frontmatter(&raw);

        let (tags, metadata) = frontmatter
            .as_ref()
            .map(parse_frontmatter)
            .unwrap_or_else(|| (Vec::new(), serde_json::Value::Object(Default::default())));

        let title = doc_ref
            .title
            .clone()
            .unwrap_or_else(|| doc_ref.external_id.clone());

        let url = {
            let vault_name = self
                .vault_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vault");
            Some(format!(
                "obsidian://open?vault={}&file={}",
                urlencoding::encode(vault_name),
                urlencoding::encode(&doc_ref.external_id)
            ))
        };

        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::Markdown(body_without_fm.to_string()),
            url,
            updated_at: doc_ref.updated_at,
            tags,
            metadata,
        })
    }

    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        let body = match &doc.body {
            DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.clone(),
            DocumentBody::NotionBlocks(_) => bail!("obsidian adapter does not handle notion blocks"),
        };
        let raw_chunks = md::chunk_markdown(&doc.title, &body, CHUNK_MAX_CHARS);
        let mut out = Vec::with_capacity(raw_chunks.len());
        for (i, c) in raw_chunks.into_iter().enumerate() {
            out.push(Chunk::new(
                doc.source_id.clone(),
                doc.external_id.clone(),
                i,
                c.text,
                c.heading_path,
                c.char_range,
                doc.updated_at,
            ));
        }
        Ok(out)
    }
}

/// Strip the YAML frontmatter block if present.
fn strip_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let mut lines = raw.split_inclusive('\n');
    let first = match lines.next() {
        Some(l) => l.trim_end_matches(['\r', '\n']),
        None => return (None, raw),
    };
    if first != "---" {
        return (None, raw);
    }
    let mut acc_len = first.len() + 1;
    let mut end_at: Option<usize> = None;
    for line in lines {
        acc_len += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            end_at = Some(acc_len);
            break;
        }
    }
    match end_at {
        Some(idx) => {
            let fm = &raw[..idx];
            let body = raw[idx..].trim_start_matches('\n').trim_start_matches('\r');
            let fm_inner = fm
                .trim_start_matches("---")
                .trim_start_matches(['\r', '\n'])
                .trim_end_matches("---\n")
                .trim_end_matches("---\r\n")
                .trim_end_matches("---");
            (Some(fm_inner), body)
        }
        None => (None, raw),
    }
}

fn parse_frontmatter(fm: &&str) -> (Vec<String>, serde_json::Value) {
    let parsed: serde_yaml::Value = serde_yaml::from_str(fm).unwrap_or(serde_yaml::Value::Null);
    let tags = parsed
        .get("tags")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let metadata = yaml_to_json(&parsed);
    (tags, metadata)
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    use serde_yaml::Value as Y;
    match v {
        Y::Null => serde_json::Value::Null,
        Y::Bool(b) => serde_json::Value::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        Y::String(s) => serde_json::Value::String(s.clone()),
        Y::Sequence(s) => serde_json::Value::Array(s.iter().map(yaml_to_json).collect()),
        Y::Mapping(m) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    obj.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            serde_json::Value::Object(obj)
        }
        Y::Tagged(t) => yaml_to_json(&t.value),
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

    #[tokio::test]
    async fn fetch_reads_file_and_parses_frontmatter() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        fs::write(
            tmp.path().join("spec.md"),
            "---\ntags: [design, oauth]\nstatus: draft\n---\n\n# Auth spec\n\nbody.",
        )
        .unwrap();

        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let doc_ref = docs.iter().find(|d| d.external_id == "spec.md").unwrap();
        let doc = adapter.fetch(doc_ref).await.unwrap();
        assert_eq!(doc.source_id, "obsidian-main");
        assert_eq!(doc.external_id, "spec.md");
        assert!(doc.tags.contains(&"design".to_string()));
        assert!(doc.tags.contains(&"oauth".to_string()));
        match &doc.body {
            DocumentBody::Markdown(s) => {
                assert!(s.starts_with("# Auth spec"));
                assert!(!s.contains("---"));
            }
            _ => panic!("expected markdown body"),
        }
        assert_eq!(doc.metadata.get("status").and_then(|v| v.as_str()), Some("draft"));
    }

    #[tokio::test]
    async fn chunk_emits_multiple_chunks_with_heading_path() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        let body = "# H1\n\npara under h1.\n\n## H2\n\npara under h2.\n\n## H2-b\n\npara under h2-b.\n";
        fs::write(tmp.path().join("multi.md"), body).unwrap();

        let inst = make_instance("obsidian-main", tmp.path());
        let adapter = ObsidianAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let doc_ref = docs.iter().find(|d| d.external_id == "multi.md").unwrap();
        let doc = adapter.fetch(doc_ref).await.unwrap();
        let chunks = adapter.chunk(&doc).unwrap();
        assert!(chunks.len() >= 3, "expected >=3 chunks, got {}", chunks.len());
        for c in &chunks {
            assert_eq!(c.source_id, "obsidian-main");
            assert_eq!(c.external_id, "multi.md");
            assert!(!c.chunk_id.is_empty());
            assert!(!c.text.is_empty());
        }
        let ords: Vec<usize> = chunks.iter().map(|c| c.ordinal).collect();
        assert_eq!(ords, (0..chunks.len()).collect::<Vec<_>>());
    }
}
