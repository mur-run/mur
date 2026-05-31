pub mod chunker;
pub mod scanner;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use arrow_array::{
    FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use chunker::chunk_file;
use scanner::scan_project;

use crate::store::embedding::{EmbeddingConfig, embed_batch};

const TABLE_NAME: &str = "chunks";
const EMBED_BATCH_SIZE: usize = 200;

/// Chunks above this threshold auto-trigger background mode.
pub const BACKGROUND_CHUNK_THRESHOLD: usize = 200;

#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub file: String,
    pub language: String,
    pub chunk_type: String,
    pub symbol: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub chunks_created: usize,
    pub duration_ms: u64,
    pub files_changed: usize,
    pub files_skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    mtime: u64,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexMetadata {
    #[serde(default)]
    pub project_path: String,
    pub files: HashMap<String, FileMeta>,
    pub last_indexed: String,
}

pub struct DiscoveredIndex {
    pub name: String,
    pub project_path: Option<String>,
    pub last_indexed: Option<String>,
    pub file_count: usize,
}

/// Written during background indexing so `mur project status` can show live progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProgress {
    pub status: IndexStatus,
    pub total_chunks: usize,
    pub done_chunks: usize,
    pub errors: usize,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IndexStatus {
    Running,
    Done,
    Error,
}

/// Lightweight lock file to prevent concurrent indexing of the same project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLock {
    pub pid: u32,
    pub project_name: String,
    pub started_at: String,
}

pub struct CodebaseIndex {
    lance_path: PathBuf,
    project_name: String,
    project_path: PathBuf,
    db: Arc<OnceCell<lancedb::Connection>>,
}

impl CodebaseIndex {
    pub fn new(project_name: &str, project_path: &Path) -> Self {
        let lance_path = crate::paths::mur_root(None)
            .join("indexes")
            .join("codebase")
            .join(format!("{project_name}.lance"));
        Self {
            lance_path,
            project_name: project_name.to_string(),
            project_path: project_path.to_path_buf(),
            db: Arc::new(OnceCell::new()),
        }
    }

    fn meta_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.meta.json", self.project_name))
    }

    fn load_meta(&self) -> Option<IndexMetadata> {
        let path = self.meta_path();
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_meta(&self, meta: &IndexMetadata) -> Result<()> {
        let path = self.meta_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(meta)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    fn lock_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.lock", self.project_name))
    }

    fn progress_path(&self) -> PathBuf {
        self.lance_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.progress.json", self.project_name))
    }

    /// Try to acquire the index lock. Returns Ok(true) if we got it, Ok(false) if another
    /// live process holds it, Err if I/O fails.
    pub fn try_acquire_lock(&self) -> Result<bool> {
        let path = self.lock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Check existing lock
        if path.exists() {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(lock) = serde_json::from_str::<IndexLock>(&data) {
                    if mur_common::lock_file::pid_alive(lock.pid) {
                        return Ok(false); // Another live process holds the lock
                    }
                    // Stale lock — pid is dead, we'll overwrite
                }
            }
        }
        let lock = IndexLock {
            pid: std::process::id(),
            project_name: self.project_name.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(&path, serde_json::to_string(&lock)?)?;
        Ok(true)
    }

    pub fn release_lock(&self) {
        let path = self.lock_path();
        let _ = std::fs::remove_file(&path);
    }

    /// Write progress for `mur project status` to read.
    pub fn write_progress(&self, progress: &IndexProgress) -> Result<()> {
        let path = self.progress_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string(progress)?)?;
        Ok(())
    }

    /// Read progress file if it exists.
    pub fn read_progress(&self) -> Option<IndexProgress> {
        let path = self.progress_path();
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    async fn get_db(&self) -> Result<&lancedb::Connection> {
        self.db
            .get_or_try_init(|| {
                let p = self.lance_path.clone();
                async move {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let s = p.to_str().unwrap_or("index.lance");
                    lancedb::connect(s)
                        .execute()
                        .await
                        .map_err(|e| anyhow::anyhow!("LanceDB connect failed: {e}"))
                }
            })
            .await
    }

    pub async fn build<F>(
        &self,
        embed_config: &EmbeddingConfig,
        rebuild: bool,
        mut on_progress: F,
    ) -> Result<IndexStats>
    where
        F: FnMut(usize, usize),
    {
        let start = Instant::now();

        let old_meta = if rebuild { None } else { self.load_meta() };

        let files = scan_project(&self.project_path);
        let files_indexed = files.len();

        let mut changed_files: Vec<&scanner::ScannedFile> = Vec::new();
        let mut unchanged_files: Vec<&scanner::ScannedFile> = Vec::new();

        for file in &files {
            let full_path = self.project_path.join(&file.relative_path);
            let is_changed = match (&old_meta, full_path.metadata().ok()) {
                (Some(meta), Some(fs_meta)) => {
                    if let Some(file_meta) = meta.files.get(&file.relative_path) {
                        let mtime = fs_meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let size = fs_meta.len();
                        file_meta.mtime != mtime || file_meta.size != size
                    } else {
                        true
                    }
                }
                _ => true,
            };
            if is_changed {
                changed_files.push(file);
            } else {
                unchanged_files.push(file);
            }
        }

        let files_changed = changed_files.len();
        let files_skipped = unchanged_files.len();

        let mut all_chunks: Vec<CodeChunk> = Vec::new();
        let mut unchanged_file_set: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for f in &unchanged_files {
            unchanged_file_set.insert(f.relative_path.as_str());
        }

        for file in &files {
            let chunks = chunk_file(&file.content, &file.language);
            for c in chunks {
                all_chunks.push(CodeChunk {
                    file: file.relative_path.clone(),
                    language: file.language.clone(),
                    chunk_type: c.chunk_type,
                    symbol: c.symbol,
                    content: c.content,
                    line_start: c.line_start,
                    line_end: c.line_end,
                    score: 0.0,
                });
            }
        }

        let chunks_created = all_chunks.len();
        if chunks_created == 0 {
            self.save_meta(&IndexMetadata::default())?;
            return Ok(IndexStats {
                files_indexed,
                chunks_created: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                files_changed,
                files_skipped,
            });
        }

        let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; chunks_created];
        let has_existing_db = self.lance_path.exists();

        // Async get_db() prevents collapsing the outer boolean guard with the inner let-Ok
        #[allow(clippy::collapsible_if)]
        if has_existing_db && !unchanged_files.is_empty() {
            if let Ok(db) = self.get_db().await {
                let table_names = db.table_names().execute().await.unwrap_or_default();
                #[allow(clippy::collapsible_if)]
                if table_names.contains(&TABLE_NAME.to_string()) {
                    if let Ok(table) = db.open_table(TABLE_NAME).execute().await {
                        let batches: Vec<RecordBatch> = match table.query().execute().await {
                            Ok(stream) => stream.try_collect().await.unwrap_or_default(),
                            Err(_) => Vec::new(),
                        };

                        let mut cache: HashMap<(String, u32, u32), Vec<f32>> = HashMap::new();
                        for batch in &batches {
                            let file_col: Option<&StringArray> = batch
                                .column_by_name("file")
                                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
                            let ls_col: Option<&UInt32Array> = batch
                                .column_by_name("line_start")
                                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
                            let le_col: Option<&UInt32Array> = batch
                                .column_by_name("line_end")
                                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
                            let vec_col: Option<&FixedSizeListArray> = batch
                                .column_by_name("vector")
                                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());

                            if let (Some(files_col), Some(ls), Some(le), Some(vecs)) =
                                (file_col, ls_col, le_col, vec_col)
                            {
                                for i in 0..batch.num_rows() {
                                    let file: String = files_col.value(i).to_string();
                                    if unchanged_file_set.contains(file.as_str()) {
                                        let values = vecs.value(i);
                                        if let Some(arr) =
                                            values.as_any().downcast_ref::<Float32Array>()
                                        {
                                            let emb: Vec<f32> = arr.values().to_vec();
                                            cache.insert((file, ls.value(i), le.value(i)), emb);
                                        }
                                    }
                                }
                            }
                        }

                        for (i, chunk) in all_chunks.iter().enumerate() {
                            if unchanged_file_set.contains(chunk.file.as_str()) {
                                let key = (chunk.file.clone(), chunk.line_start, chunk.line_end);
                                if let Some(emb) = cache.get(&key) {
                                    embeddings[i] = Some(emb.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        let chunks_to_embed: Vec<usize> = embeddings
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none())
            .map(|(i, _)| i)
            .collect();

        let total_to_embed = chunks_to_embed.len();

        if total_to_embed > 0 {
            on_progress(0, total_to_embed);
        }
        for batch_start in (0..total_to_embed).step_by(EMBED_BATCH_SIZE) {
            let batch_end = (batch_start + EMBED_BATCH_SIZE).min(total_to_embed);
            let batch_indices = &chunks_to_embed[batch_start..batch_end];

            let texts: Vec<String> = batch_indices
                .iter()
                .map(|&idx| {
                    let c = &all_chunks[idx];
                    let prefix = match &c.symbol {
                        Some(sym) => format!("{} {} {}: ", c.file, c.language, sym),
                        None => format!("{} {}: ", c.file, c.language),
                    };
                    let max_content = 2000;
                    let content = if c.content.len() > max_content {
                        let mut end = max_content;
                        while end > 0 && !c.content.is_char_boundary(end) {
                            end -= 1;
                        }
                        &c.content[..end]
                    } else {
                        &c.content
                    };
                    format!("{prefix}{content}")
                })
                .collect();

            let batch_embeddings = embed_batch(&texts, embed_config)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            for (j, emb) in batch_embeddings.into_iter().enumerate() {
                embeddings[batch_indices[j]] = Some(emb);
            }

            on_progress(batch_end, total_to_embed);
        }

        if total_to_embed == 0 {
            on_progress(0, 0);
        }

        let final_embeddings: Vec<Vec<f32>> = embeddings
            .into_iter()
            .map(|e| e.unwrap_or_default())
            .collect();

        if final_embeddings.iter().any(|e| e.is_empty()) {
            anyhow::bail!("Some chunks failed to get embeddings");
        }

        let db = self.get_db().await?;

        let table_names = db.table_names().execute().await?;
        if table_names.contains(&TABLE_NAME.to_string()) {
            db.drop_table(TABLE_NAME, &[]).await?;
        }

        let dim = final_embeddings[0].len() as i32;
        let schema = codebase_schema(dim);

        let id_values: Vec<String> = (0..chunks_created)
            .map(|i| format!("{}:{}:{}", self.project_name, all_chunks[i].file, i))
            .collect();
        let file_values: Vec<&str> = all_chunks.iter().map(|c| c.file.as_str()).collect();
        let lang_values: Vec<&str> = all_chunks.iter().map(|c| c.language.as_str()).collect();
        let type_values: Vec<&str> = all_chunks.iter().map(|c| c.chunk_type.as_str()).collect();
        let symbol_values: Vec<String> = all_chunks
            .iter()
            .map(|c| c.symbol.clone().unwrap_or_default())
            .collect();
        let symbol_refs: Vec<&str> = symbol_values.iter().map(|s| s.as_str()).collect();
        let content_values: Vec<&str> = all_chunks.iter().map(|c| c.content.as_str()).collect();
        let line_start_values: Vec<u32> = all_chunks.iter().map(|c| c.line_start).collect();
        let line_end_values: Vec<u32> = all_chunks.iter().map(|c| c.line_end).collect();

        let flat_vectors: Vec<f32> = final_embeddings.iter().flatten().copied().collect();
        let values_array = Float32Array::from(flat_vectors);
        let field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = FixedSizeListArray::try_new(field, dim, Arc::new(values_array), None)?;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(id_values)),
                Arc::new(StringArray::from(file_values)),
                Arc::new(StringArray::from(lang_values)),
                Arc::new(StringArray::from(type_values)),
                Arc::new(StringArray::from(symbol_refs)),
                Arc::new(StringArray::from(content_values)),
                Arc::new(UInt32Array::from(line_start_values)),
                Arc::new(UInt32Array::from(line_end_values)),
                Arc::new(vector_array),
            ],
        )?;

        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
        db.create_table(TABLE_NAME, Box::new(reader))
            .execute()
            .await?;

        let mut new_meta = IndexMetadata {
            project_path: self.project_path.display().to_string(),
            files: HashMap::new(),
            last_indexed: chrono::Utc::now().to_rfc3339(),
        };
        for file in &files {
            let full_path = self.project_path.join(&file.relative_path);
            if let Ok(fs_meta) = full_path.metadata() {
                let mtime = fs_meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                new_meta.files.insert(
                    file.relative_path.clone(),
                    FileMeta {
                        mtime,
                        size: fs_meta.len(),
                    },
                );
            }
        }
        self.save_meta(&new_meta)?;

        Ok(IndexStats {
            files_indexed,
            chunks_created,
            duration_ms: start.elapsed().as_millis() as u64,
            files_changed,
            files_skipped,
        })
    }

    pub async fn search(&self, query_embedding: &[f32], limit: usize) -> Result<Vec<CodeChunk>> {
        let db = self.get_db().await?;

        let table_names = db.table_names().execute().await?;
        if !table_names.contains(&TABLE_NAME.to_string()) {
            return Ok(Vec::new());
        }

        let table = db.open_table(TABLE_NAME).execute().await?;
        let batches: Vec<RecordBatch> = table
            .vector_search(query_embedding)?
            .distance_type(lancedb::DistanceType::Cosine)
            .limit(limit)
            .execute()
            .await?
            .try_collect()
            .await?;

        let mut results = Vec::new();
        for batch in &batches {
            let file_col = batch
                .column_by_name("file")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let lang_col = batch
                .column_by_name("language")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let type_col = batch
                .column_by_name("chunk_type")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let symbol_col = batch
                .column_by_name("symbol")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let content_col = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let line_start_col = batch
                .column_by_name("line_start")
                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
            let line_end_col = batch
                .column_by_name("line_end")
                .and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
            let dist_col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            let Some(files) = file_col else { continue };
            let Some(langs) = lang_col else { continue };
            let Some(types) = type_col else { continue };
            let Some(contents) = content_col else {
                continue;
            };

            for i in 0..batch.num_rows() {
                let symbol = symbol_col
                    .map(|s| s.value(i).to_string())
                    .filter(|s| !s.is_empty());
                let score = dist_col.map_or(0.0, |d| 1.0 - d.value(i));

                results.push(CodeChunk {
                    file: files.value(i).to_string(),
                    language: langs.value(i).to_string(),
                    chunk_type: types.value(i).to_string(),
                    symbol,
                    content: contents.value(i).to_string(),
                    line_start: line_start_col.map_or(0, |c| c.value(i)),
                    line_end: line_end_col.map_or(0, |c| c.value(i)),
                    score,
                });
            }
        }

        Ok(results)
    }

    pub async fn stats_async(&self) -> Result<IndexStats> {
        if !self.lance_path.exists() {
            return Ok(IndexStats {
                files_indexed: 0,
                chunks_created: 0,
                duration_ms: 0,
                files_changed: 0,
                files_skipped: 0,
            });
        }

        let db = self.get_db().await?;
        let table_names = db.table_names().execute().await?;
        if !table_names.contains(&TABLE_NAME.to_string()) {
            return Ok(IndexStats {
                files_indexed: 0,
                chunks_created: 0,
                duration_ms: 0,
                files_changed: 0,
                files_skipped: 0,
            });
        }

        let table = db.open_table(TABLE_NAME).execute().await?;
        let count = table.count_rows(None).await?;

        Ok(IndexStats {
            files_indexed: 0,
            chunks_created: count,
            duration_ms: 0,
            files_changed: 0,
            files_skipped: 0,
        })
    }

    // ── Getters (pub API for external consumers) ─────────────────

    #[allow(dead_code)]
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    #[allow(dead_code)]
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub fn lance_path(&self) -> &Path {
        &self.lance_path
    }
}

pub fn discover_all_indexes() -> Vec<DiscoveredIndex> {
    let indexes_dir = crate::paths::mur_root(None)
        .join("indexes")
        .join("codebase");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".lance") || !path.is_dir() {
            continue;
        }
        let project_name = name.trim_end_matches(".lance");
        let meta_path = indexes_dir.join(format!("{project_name}.meta.json"));
        let (project_path, last_indexed, file_count) =
            if let Ok(data) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<IndexMetadata>(&data) {
                    let pp = if meta.project_path.is_empty() {
                        None
                    } else {
                        Some(meta.project_path)
                    };
                    (pp, Some(meta.last_indexed), meta.files.len())
                } else {
                    (None, None, 0)
                }
            } else {
                (None, None, 0)
            };

        results.push(DiscoveredIndex {
            name: project_name.to_string(),
            project_path,
            last_indexed,
            file_count,
        });
    }
    results.sort_by_key(|a| a.name.to_lowercase());
    results
}

pub fn ensure_git_hook(project_path: &Path, quiet: bool) -> Result<bool> {
    let hooks_dir = project_path.join(".git").join("hooks");
    if !hooks_dir.exists() {
        return Ok(false);
    }
    let hook_path = hooks_dir.join("post-commit");
    let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();
    let marker = "# mur auto-index";
    if existing.contains(marker) {
        return Ok(false);
    }
    let mur_bin = dirs::home_dir()
        .map(|d| d.join(".mur").join("bin").join("mur"))
        .unwrap_or_else(|| PathBuf::from("mur"));
    let hook_content = format!(
        "\n{}\nif command -v {} &>/dev/null; then\n  {} project index \"{}\" --quiet &\nfi\n",
        marker,
        mur_bin.display(),
        mur_bin.display(),
        project_path.display(),
    );
    if existing.is_empty() {
        std::fs::write(&hook_path, format!("#!/bin/sh\n{}", hook_content))?;
    } else {
        let mut file = std::fs::OpenOptions::new().append(true).open(&hook_path)?;
        use std::io::Write;
        file.write_all(hook_content.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    if !quiet {
        eprintln!("  Git hook installed for auto-reindex on commit");
    }
    Ok(true)
}

fn codebase_schema(dim: i32) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("file", DataType::Utf8, false),
        Field::new("language", DataType::Utf8, false),
        Field::new("chunk_type", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("line_start", DataType::UInt32, false),
        Field::new("line_end", DataType::UInt32, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            false,
        ),
    ]))
}
