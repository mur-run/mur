//! Joplin adapter.
//!
//! Two modes:
//!  - Local SQLite: reads `database.sqlite` directly, opened read-only with
//!    `?mode=ro&immutable=1` URI flags so a running Joplin app does not
//!    cause lock contention.
//!  - Joplin Server: REST API with bearer token from keyring.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use std::path::PathBuf;

use crate::sources::KnowledgeSource;
use crate::sources::chunker::markdown as md;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

const CHUNK_MAX_CHARS: usize = 6000;

pub enum JoplinMode {
    LocalDb { db_path: PathBuf },
    Server { url: String, token: String },
}

pub struct JoplinAdapter {
    id: String,
    mode: JoplinMode,
    weight: f32,
}

impl JoplinAdapter {
    pub fn from_instance(instance: &SourceInstance, server_token: Option<String>) -> Result<Self> {
        if instance.type_name != "joplin" {
            bail!("expected type_name 'joplin', got '{}'", instance.type_name);
        }
        if let Some(server_url) = instance.scope.get("server_url").and_then(|v| v.as_str()) {
            let token = server_token.context("joplin server mode needs a token")?;
            return Ok(Self {
                id: instance.id.clone(),
                mode: JoplinMode::Server {
                    url: server_url.to_string(),
                    token,
                },
                weight: instance.weight,
            });
        }
        if let Some(db_path) = instance.scope.get("db_path").and_then(|v| v.as_str()) {
            let p = PathBuf::from(db_path);
            if !p.exists() {
                bail!("joplin db not found: {}", p.display());
            }
            return Ok(Self {
                id: instance.id.clone(),
                mode: JoplinMode::LocalDb { db_path: p },
                weight: instance.weight,
            });
        }
        bail!("joplin source needs scope.db_path or scope.server_url");
    }

    fn open_ro(db_path: &std::path::Path) -> Result<Connection> {
        let uri = format!("file:{}?mode=ro&immutable=1", db_path.display());
        Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("open joplin sqlite at {}", db_path.display()))
    }

    async fn list_local(&self, cursor: Option<SyncCursor>) -> Result<(Vec<DocRef>, SyncCursor)> {
        let db_path = match &self.mode {
            JoplinMode::LocalDb { db_path } => db_path.clone(),
            _ => bail!("list_local called on non-local mode"),
        };
        let cursor_in = cursor.clone();
        let (docs, max_ms) = tokio::task::spawn_blocking(move || {
            let conn = Self::open_ro(&db_path)?;
            let threshold_ms: Option<i64> = cursor_in.and_then(|c| {
                if c.is_empty() {
                    None
                } else {
                    DateTime::parse_from_rfc3339(&c.0)
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                }
            });

            let mut sql = String::from(
                "SELECT id, title, updated_time FROM notes \
                 WHERE is_conflict = 0 AND COALESCE(deleted_time, 0) = 0",
            );
            let mut params: Vec<i64> = Vec::new();
            if let Some(t) = threshold_ms {
                sql.push_str(" AND updated_time > ?1");
                params.push(t);
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
                let id: String = row.get(0)?;
                let title: Option<String> = row.get(1).ok();
                let updated_time_ms: i64 = row.get(2)?;
                Ok((id, title, updated_time_ms))
            })?;
            let mut docs: Vec<DocRef> = Vec::new();
            let mut max_ms: i64 = threshold_ms.unwrap_or(0);
            for r in rows {
                let (id, title, updated_ms) = r?;
                if updated_ms > max_ms {
                    max_ms = updated_ms;
                }
                let updated_at = DateTime::<Utc>::from_timestamp_millis(updated_ms)
                    .unwrap_or_else(Utc::now);
                docs.push(DocRef {
                    external_id: id,
                    title,
                    updated_at,
                });
            }
            Ok::<_, anyhow::Error>((docs, max_ms))
        })
        .await
        .context("spawn_blocking joplin list")??;
        let cursor_out = if max_ms > 0 {
            SyncCursor(
                DateTime::<Utc>::from_timestamp_millis(max_ms)
                    .unwrap_or_else(Utc::now)
                    .to_rfc3339(),
            )
        } else {
            SyncCursor(String::new())
        };
        Ok((docs, cursor_out))
    }

    async fn list_server(
        &self,
        url: &str,
        token: &str,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        let threshold_ms: Option<i64> = cursor.and_then(|c| {
            DateTime::parse_from_rfc3339(&c.0)
                .ok()
                .map(|dt| dt.timestamp_millis())
        });
        let client = reqwest::Client::new();
        let mut docs: Vec<DocRef> = Vec::new();
        let mut max_ms: i64 = threshold_ms.unwrap_or(0);
        let mut page = 1;
        loop {
            let req_url = format!(
                "{}/api/items?type=note&token={}&fields=id,title,updated_time&page={}",
                url.trim_end_matches('/'),
                token,
                page
            );
            let resp = client
                .get(&req_url)
                .send()
                .await
                .context("joplin server list")?;
            if !resp.status().is_success() {
                bail!(
                    "joplin server returned {}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                );
            }
            let v: serde_json::Value = resp.json().await?;
            let items = v
                .get("items")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                break;
            }
            for item in items {
                let id = item
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = item
                    .get("title")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                let updated_ms = item
                    .get("updated_time")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0);
                if let Some(t) = threshold_ms
                    && updated_ms <= t
                {
                    continue;
                }
                if updated_ms > max_ms {
                    max_ms = updated_ms;
                }
                let updated_at = DateTime::<Utc>::from_timestamp_millis(updated_ms)
                    .unwrap_or_else(Utc::now);
                docs.push(DocRef {
                    external_id: id,
                    title,
                    updated_at,
                });
            }
            let has_more = v
                .get("has_more")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if !has_more {
                break;
            }
            page += 1;
        }
        let cursor_out = if max_ms > 0 {
            SyncCursor(
                DateTime::<Utc>::from_timestamp_millis(max_ms)
                    .unwrap_or_else(Utc::now)
                    .to_rfc3339(),
            )
        } else {
            SyncCursor(String::new())
        };
        Ok((docs, cursor_out))
    }

    async fn fetch_server(&self, url: &str, token: &str, doc_id: &str) -> Result<String> {
        let req_url = format!(
            "{}/api/items/{}?token={}&fields=body",
            url.trim_end_matches('/'),
            doc_id,
            token
        );
        let client = reqwest::Client::new();
        let resp = client.get(&req_url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "joplin fetch failed {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("body")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }
}

#[async_trait]
impl KnowledgeSource for JoplinAdapter {
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
        match &self.mode {
            JoplinMode::LocalDb { .. } => self.list_local(cursor).await,
            JoplinMode::Server { url, token } => {
                self.list_server(url, token, cursor).await
            }
        }
    }

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        let body_text = match &self.mode {
            JoplinMode::LocalDb { db_path } => {
                let db_path = db_path.clone();
                let id = doc_ref.external_id.clone();
                tokio::task::spawn_blocking(move || -> Result<String> {
                    let conn = Self::open_ro(&db_path)?;
                    let mut stmt = conn.prepare("SELECT body FROM notes WHERE id = ?1")?;
                    let body: String = stmt.query_row([&id], |row| row.get::<_, String>(0))?;
                    Ok(body)
                })
                .await
                .context("spawn_blocking joplin fetch")??
            }
            JoplinMode::Server { url, token } => {
                self.fetch_server(url, token, &doc_ref.external_id).await?
            }
        };
        let title = doc_ref
            .title
            .clone()
            .unwrap_or_else(|| doc_ref.external_id.clone());
        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::Markdown(body_text),
            url: Some(format!(
                "joplin://x-callback-url/openNote?id={}",
                doc_ref.external_id
            )),
            updated_at: doc_ref.updated_at,
            tags: vec![],
            metadata: serde_json::Value::Null,
        })
    }

    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        let body = match &doc.body {
            DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.clone(),
            DocumentBody::NotionBlocks(_) => bail!("joplin adapter does not handle notion blocks"),
        };
        let raw = md::chunk_markdown(&doc.title, &body, CHUNK_MAX_CHARS);
        let mut out = Vec::with_capacity(raw.len());
        for (i, c) in raw.into_iter().enumerate() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn make_test_db(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("database.sqlite");
        let conn = Connection::open(&p).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                title TEXT,
                body TEXT,
                updated_time INTEGER NOT NULL,
                is_conflict INTEGER DEFAULT 0,
                deleted_time INTEGER DEFAULT 0
            );
            INSERT INTO notes (id, title, body, updated_time) VALUES
                ('n1', 'note 1', '# h1\n\nbody 1', 1700000000000),
                ('n2', 'note 2', 'plain body', 1700000005000);
            INSERT INTO notes (id, title, body, updated_time, is_conflict) VALUES
                ('n3', 'conflict', 'x', 1700000010000, 1);",
        )
        .unwrap();
        p
    }

    fn make_instance(db_path: &std::path::Path) -> SourceInstance {
        let mut scope = BTreeMap::new();
        scope.insert(
            "db_path".into(),
            serde_yaml::Value::String(db_path.to_string_lossy().to_string()),
        );
        SourceInstance {
            id: "joplin:test".into(),
            type_name: "joplin".into(),
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
    async fn list_documents_skips_conflicts() {
        let tmp = TempDir::new().unwrap();
        let db = make_test_db(tmp.path());
        let inst = make_instance(&db);
        let adapter = JoplinAdapter::from_instance(&inst, None).unwrap();
        let (docs, _cursor) = adapter.list_documents(None).await.unwrap();
        assert_eq!(docs.len(), 2);
        assert!(docs.iter().any(|d| d.external_id == "n1"));
        assert!(docs.iter().any(|d| d.external_id == "n2"));
        assert!(!docs.iter().any(|d| d.external_id == "n3"));
    }

    #[tokio::test]
    async fn fetch_returns_body() {
        let tmp = TempDir::new().unwrap();
        let db = make_test_db(tmp.path());
        let inst = make_instance(&db);
        let adapter = JoplinAdapter::from_instance(&inst, None).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let n1 = docs.iter().find(|d| d.external_id == "n1").unwrap();
        let doc = adapter.fetch(n1).await.unwrap();
        match &doc.body {
            DocumentBody::Markdown(s) => assert!(s.contains("# h1")),
            _ => panic!("expected markdown"),
        }
    }
}
