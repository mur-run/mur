use anyhow::Result;
use chrono::Utc;
use mur_common::action::{ActionEvent, ItemSource, PendingFile, PendingItem, PendingStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::ledger::ActionLedger;
use super::{Pipeline, PipelineError};

/// In-memory pending-item store. Backed by the JSONL ledger + a
/// periodic snapshot (`pending.json`) rebuilt from ledger replay.
pub struct PendingStore {
    pipeline: Pipeline,
    items: HashMap<Uuid, PendingItem>,
    /// (canonical_path, ingested_at) for dedup within 5s
    recent_paths: HashMap<PathBuf, Instant>,
    /// Last ingestion timestamp for merge window
    last_ingest: Option<Instant>,
    last_ingest_item_id: Option<Uuid>,
    ledger: ActionLedger,
}

const DEDUP_WINDOW: Duration = Duration::from_secs(5);
const MERGE_WINDOW: Duration = Duration::from_secs(5);

impl PendingStore {
    pub fn new(pipeline: &Pipeline) -> Result<Self> {
        let _ledger = ActionLedger::open(&pipeline.ledger_dir())?;
        // Rebuild from ledger
        let events = ActionLedger::replay_today(&pipeline.ledger_dir());
        let mut items: HashMap<Uuid, PendingItem> = HashMap::new();
        let mut recent_paths: HashMap<PathBuf, Instant> = HashMap::new();
        let mut last_ingest: Option<Instant> = None;
        let mut last_ingest_item_id: Option<Uuid> = None;

        for event in &events {
            match event {
                ActionEvent::ItemIngested { item } => {
                    let now = Utc::now();
                    let elapsed = (now - item.created_at).num_seconds();
                    if elapsed < pipeline.config.queue.pending_item_ttl_minutes as i64 * 60 {
                        last_ingest = Some(Instant::now());
                        last_ingest_item_id = Some(item.id);
                        for f in &item.files {
                            let canonical = canonicalize_path(&f.path);
                            recent_paths.insert(canonical, Instant::now());
                        }
                        items.insert(item.id, item.clone());
                    }
                }
                ActionEvent::ItemSelected { item_id, .. } => {
                    items.remove(item_id);
                }
                ActionEvent::ItemExpired { item_id } => {
                    items.remove(item_id);
                }
                _ => {}
            }
        }

        Ok(Self {
            pipeline: pipeline.clone(),
            items,
            recent_paths,
            last_ingest,
            last_ingest_item_id,
            ledger: _ledger,
        })
    }

    /// Ingest files, applying dedup and merge rules.
    pub fn ingest_files(
        &mut self,
        source: ItemSource,
        paths: Vec<PathBuf>,
    ) -> Result<PendingItem, PipelineError> {
        let now = Instant::now();

        // Dedup: same canonical path within DEDUP_WINDOW
        for path in &paths {
            let canonical = canonicalize_path(path);
            if let Some(t) = self.recent_paths.get(&canonical)
                && now.duration_since(*t) < DEDUP_WINDOW
            {
                // Return existing item — same batch
                if let Some(id) = self.last_ingest_item_id
                    && let Some(item) = self.items.get(&id)
                {
                    return Ok(item.clone());
                }
            }
        }

        // Merge: if within MERGE_WINDOW of last ingest, extend existing batch
        if let Some(last) = self.last_ingest
            && now.duration_since(last) < MERGE_WINDOW
            && let Some(existing_id) = self.last_ingest_item_id
        {
            // Compute file info before mutable borrow
            let mut new_files = Vec::new();
            for path in &paths {
                let mime_type = self
                    .detect_mime(path)
                    .unwrap_or_else(|_| "application/octet-stream".into());
                let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                new_files.push(PendingFile {
                    path: path.clone(),
                    mime_type,
                    size_bytes,
                    thumbnail_path: None,
                });
            }

            let merged = if let Some(existing) = self.items.get_mut(&existing_id) {
                existing.files.extend(new_files);
                existing.created_at = Utc::now();
                existing.clone()
            } else {
                return Err(PipelineError::PendingNotFound {
                    item_id: existing_id.to_string(),
                });
            };

            let event = ActionEvent::ItemIngested {
                item: merged.clone(),
            };
            self.ledger.append(&event)?;
            self.write_snapshot()?;
            return Ok(merged);
        }

        // New batch
        let item = self.create_pending_item(source, paths, now)?;
        self.ledger
            .append(&ActionEvent::ItemIngested { item: item.clone() })?;
        self.write_snapshot()?;
        Ok(item)
    }

    fn create_pending_item(
        &mut self,
        source: ItemSource,
        paths: Vec<PathBuf>,
        now: Instant,
    ) -> Result<PendingItem, PipelineError> {
        let id = Uuid::now_v7();
        let mut files = Vec::new();
        for path in &paths {
            let mime_type = self
                .detect_mime(path)
                .unwrap_or_else(|_| "application/octet-stream".into());
            let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            files.push(PendingFile {
                path: path.clone(),
                mime_type,
                size_bytes,
                thumbnail_path: None,
            });
            self.recent_paths.insert(canonicalize_path(path), now);
        }

        let item = PendingItem {
            id,
            source,
            files,
            created_at: Utc::now(),
            status: PendingStatus::AwaitingSelection,
        };

        self.items.insert(id, item.clone());
        self.last_ingest = Some(now);
        self.last_ingest_item_id = Some(id);
        Ok(item)
    }

    /// Detect MIME type via magic bytes. Falls back to extension.
    pub fn detect_mime(&self, path: &Path) -> Result<String> {
        let mut buf = [0u8; 512];
        let len = if let Ok(mut f) = std::fs::File::open(path) {
            use std::io::Read;
            f.read(&mut buf).unwrap_or(0)
        } else {
            0
        };

        // PDF magic
        if len >= 5 && &buf[..5] == b"%PDF-" {
            return Ok("application/pdf".into());
        }
        // PNG
        if len >= 8 && &buf[..8] == b"\x89PNG\r\n\x1a\n" {
            return Ok("image/png".into());
        }
        // JPEG
        if len >= 2 && &buf[..2] == b"\xff\xd8" {
            return Ok("image/jpeg".into());
        }
        // GIF
        if len >= 6 && (&buf[..6] == b"GIF87a" || &buf[..6] == b"GIF89a") {
            return Ok("image/gif".into());
        }
        // ZIP-based formats
        if len >= 4 && &buf[..4] == b"PK\x03\x04" {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                return Ok(match ext.to_lowercase().as_str() {
                    "docx" => {
                        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                            .into()
                    }
                    "xlsx" => {
                        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into()
                    }
                    "pptx" => {
                        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                            .into()
                    }
                    _ => "application/zip".into(),
                });
            }
            return Ok("application/zip".into());
        }

        // Fallback to extension
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            return Ok(mime_guess::from_ext(ext)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .into());
        }

        Ok("application/octet-stream".into())
    }

    /// Mark an item as expired and remove from the pending set.
    pub fn expire_item(&mut self, item_id: Uuid) -> Result<()> {
        if let Some(item) = self.items.get_mut(&item_id) {
            item.status = PendingStatus::Expired;
        }
        self.items.remove(&item_id);
        self.ledger.append(&ActionEvent::ItemExpired { item_id })?;
        self.write_snapshot()?;
        Ok(())
    }

    /// Select an action for a pending item.
    pub fn select_action(
        &mut self,
        item_id: Uuid,
        action: mur_common::action::Action,
    ) -> Result<()> {
        if let Some(item) = self.items.get_mut(&item_id) {
            item.status = PendingStatus::Selected {
                action_id: action.id.clone(),
            };
            self.ledger
                .append(&ActionEvent::ItemSelected { item_id, action })?;
            self.write_snapshot()?;
        }
        Ok(())
    }

    /// Current snapshot of pending items.
    pub fn snapshot(&self) -> Vec<&PendingItem> {
        self.items.values().collect()
    }

    /// Expire items past their TTL.
    pub fn expire_stale(&mut self) -> Result<usize> {
        let ttl = self.pipeline.config.queue.pending_item_ttl_minutes as i64;
        let cutoff = Utc::now() - chrono::Duration::minutes(ttl);
        let mut count = 0;
        let expired: Vec<Uuid> = self
            .items
            .iter()
            .filter(|(_, item)| item.created_at < cutoff)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.expire_item(id)?;
            count += 1;
        }
        Ok(count)
    }

    /// Write pending.json snapshot (temp + rename for atomicity).
    fn write_snapshot(&self) -> Result<()> {
        let path = self.pipeline.pending_snapshot_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let snapshot: Vec<_> = self.items.values().cloned().collect();
        let json = serde_json::to_vec_pretty(&snapshot)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    fn temp_pipeline() -> (Pipeline, TempDir) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("agent_home");
        std::fs::create_dir_all(&home).unwrap();
        let pipeline = Pipeline::new(
            home.clone(),
            mur_common::action::ActionPipelineConfig::default(),
        );
        (pipeline, tmp)
    }

    #[test]
    fn ingest_single_file_creates_pending_item() {
        let (pipeline, _tmp) = temp_pipeline();
        let src = make_file(_tmp.path(), "test.txt", b"hello world");

        let mut store = PendingStore::new(&pipeline).unwrap();
        let item = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![src.clone()],
                },
                vec![src],
            )
            .unwrap();

        assert_eq!(item.files.len(), 1);
        assert_eq!(item.status, PendingStatus::AwaitingSelection);
        // Verify written to ledger
        let events = ActionLedger::replay_today(&pipeline.ledger_dir());
        assert_eq!(events.len(), 1);
        match &events[0] {
            ActionEvent::ItemIngested { item: ev_item } => {
                assert_eq!(ev_item.id, item.id);
            }
            _ => panic!("expected ItemIngested"),
        }
    }

    #[test]
    fn mime_detect_pdf_via_magic_bytes() {
        let (pipeline, _tmp) = temp_pipeline();
        let pdf = make_file(_tmp.path(), "test.pdf", b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");

        let store = PendingStore::new(&pipeline).unwrap();
        let mime = store.detect_mime(&pdf).unwrap();
        assert_eq!(mime, "application/pdf");
    }

    #[test]
    fn mime_detect_text_file() {
        let (pipeline, _tmp) = temp_pipeline();
        let txt = make_file(_tmp.path(), "test.txt", b"plain text");

        let store = PendingStore::new(&pipeline).unwrap();
        let mime = store.detect_mime(&txt).unwrap();
        assert!(mime.starts_with("text/plain"), "got {mime}");
    }

    #[test]
    fn dedup_same_path_within_5s_returns_existing() {
        let (pipeline, _tmp) = temp_pipeline();
        let src = make_file(_tmp.path(), "test.txt", b"hello");

        let mut store = PendingStore::new(&pipeline).unwrap();
        let item1 = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![src.clone()],
                },
                vec![src.clone()],
            )
            .unwrap();

        // Within 5s, same path → dedup, return existing item
        let item2 = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![src.clone()],
                },
                vec![src],
            )
            .unwrap();
        assert_eq!(item1.id, item2.id, "same path within 5s must dedup");
    }

    #[test]
    fn merge_new_files_within_5s_extends_batch() {
        let (pipeline, _tmp) = temp_pipeline();
        let f1 = make_file(_tmp.path(), "a.txt", b"a");
        let f2 = make_file(_tmp.path(), "b.txt", b"b");

        let mut store = PendingStore::new(&pipeline).unwrap();
        let item1 = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![f1.clone()],
                },
                vec![f1],
            )
            .unwrap();

        let item2 = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![f2.clone()],
                },
                vec![f2],
            )
            .unwrap();

        // Same PendingItem (merged), file count increased
        assert_eq!(item1.id, item2.id);
        assert_eq!(item2.files.len(), 2);
    }

    #[test]
    fn expired_items_are_removed_from_snapshot() {
        let (pipeline, _tmp) = temp_pipeline();
        let src = make_file(_tmp.path(), "test.txt", b"hi");

        let mut store = PendingStore::new(&pipeline).unwrap();
        let item = store
            .ingest_files(
                ItemSource::DragDrop {
                    paths: vec![src.clone()],
                },
                vec![src],
            )
            .unwrap();

        // Manually expire it
        store.expire_item(item.id).unwrap();

        let snapshot = store.snapshot();
        assert!(!snapshot.iter().any(|i| i.id == item.id));
    }
}
