# Project Index Background Execution + Batch Fix

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `mur project index` taking hours by (1) making `embed_batch` actually batch for OpenAI-compatible providers, and (2) auto-detecting large indexing jobs and running them in background with progress file + desktop notification.

**Architecture:** Three layers: (a) fix `embed_batch` OpenAI path to send all texts in one HTTP request like Ollama already does, (b) add a hidden `project-index-worker` internal subcommand that does the actual work and writes progress/lock files, (c) `project index` CLI auto-detects chunk count > 200 and spawns the worker in background, returning immediately. `mur project status` reads the progress file for live status. Git hook updated to use `--background` for lock-file protection.

**Tech Stack:** Rust (edition 2024), reqwest, serde_json, `osascript` (macOS notification), existing LanceDB + arrow stack.

---

## File Map

| File | Role |
|------|------|
| `mur-core/src/store/embedding.rs` | Fix `embed_batch` OpenAI path — true batch API call |
| `mur-core/src/cmd/reindex.rs` | Switch `cmd_reindex` from per-item `embed()` to `embed_batch()` |
| `mur-core/src/codebase/mod.rs` | Add `IndexProgress`, `IndexLock`, lock/progress file I/O, background spawn logic, threshold constant |
| `mur-core/src/cmd/project.rs` | Add `--background`/`--foreground` flags, auto-detect logic, `project-index-worker` hidden subcommand |
| `mur-core/src/cli/mod.rs` | Add `BackgroundMode` enum + CLI args; add hidden `ProjectIndexWorker` subcommand |
| `mur-core/src/dispatch.rs` | Wire new subcommand, pass background mode to `cmd_project_index` |
| `mur-core/src/codebase/mod.rs` (git hook) | Update `ensure_git_hook` to use `--background` |

---

### Task 1: Fix `embed_batch` OpenAI path — true batch API call

**Files:**
- Modify: `mur-core/src/store/embedding.rs:159-201`

The OpenAI embeddings API accepts `input: string | string[]`. We're sending one string per request. Fix: accept `Vec<String>` and send all at once.

- [ ] **Step 1: Change `OpenAIEmbedRequest.input` to accept `Vec<String>`**

Replace lines 159-163:
```rust
#[derive(Serialize)]
struct OpenAIEmbedRequest {
    model: String,
    input: Vec<String>,
}
```

- [ ] **Step 2: Add `embed_openai_batch()` function**

Insert after `embed_openai()` (after line 201):

```rust
async fn embed_openai_batch(
    texts: &[String],
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: texts.to_vec(),
        })
        .send()
        .await
        .with_context(|| format!("calling embed API at {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Embed API error {} at {}: {}", status, url, body);
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing embed response")?;
    let embeddings: Vec<Vec<f32>> = data.data.into_iter().map(|d| d.embedding).collect();
    if embeddings.len() != texts.len() {
        anyhow::bail!(
            "Embed API returned {} embeddings but {} were requested",
            embeddings.len(),
            texts.len()
        );
    }
    Ok(embeddings)
}
```

- [ ] **Step 3: Wire `embed_openai_batch` into `embed_batch`**

Replace lines 83-89 in `embed_batch`:
```rust
// Before:
EmbeddingProvider::OpenAI { .. } => {
    let mut results = Vec::with_capacity(texts.len());
    for text in texts {
        results.push(embed(text, config).await?);
    }
    Ok(results)
}

// After:
EmbeddingProvider::OpenAI { api_key, base_url } => {
    embed_openai_batch(texts, base_url, api_key, &config.model).await
}
```

- [ ] **Step 4: Build check**

```bash
cargo build -p mur-core 2>&1 | head -20
```
Expected: compiles cleanly (no signature changes to public API).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/store/embedding.rs
git commit -m "fix(embed): make embed_batch actually batch for OpenAI-compatible providers

OpenAI embeddings API accepts input: string[] but we were sending one
string per HTTP request in a loop. Now sends all texts in a single
batch request, matching the Ollama path behavior.

This is a 10-100x speedup for providers like omlx that use the OpenAI
API path with large embedding models."
```

---

### Task 2: Fix `cmd_reindex` to use `embed_batch` instead of per-item `embed`

**Files:**
- Modify: `mur-core/src/cmd/reindex.rs:43-91`

`cmd_reindex` loops through patterns and workflows one at a time calling `embed()`. Change to collect all texts, call `embed_batch()` once (or in chunks), then pair results back.

- [ ] **Step 1: Replace sequential embed loop with batch**

Replace lines 38-91 (the two for-loops and surrounding variables):

```rust
    // Collect all texts first
    struct Item {
        is_workflow: bool,
        index: usize,
        name: String,
    }
    let mut texts: Vec<String> = Vec::new();
    let mut items: Vec<Item> = Vec::new();

    for (i, pattern) in patterns.iter().enumerate() {
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
        texts.push(text);
        items.push(Item { is_workflow: false, index: i, name: pattern.name.clone() });
    }

    let pattern_count = patterns.len();
    for (i, workflow) in workflows.iter().enumerate() {
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
        texts.push(text);
        items.push(Item { is_workflow: true, index: i, name: workflow.name.clone() });
    }

    let total = texts.len();
    let mut errors = 0;

    // Batch embed — use same batch size as project index
    const EMBED_BATCH: usize = 200;
    let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(total);

    for batch_start in (0..total).step_by(EMBED_BATCH) {
        let batch_end = (batch_start + EMBED_BATCH).min(total);
        let batch_texts = &texts[batch_start..batch_end];
        match embed_batch(&batch_texts.iter().cloned().collect::<Vec<_>>(), &config).await {
            Ok(batch_embs) => {
                all_embeddings.extend(batch_embs);
                println!("  {}/{} embedded...", batch_end, total);
            }
            Err(e) => {
                eprintln!("  ⚠️  batch {}-{} embedding failed: {}", batch_start, batch_end, e);
                errors += batch_end - batch_start;
                // Fill with zeros so indices stay aligned
                for _ in batch_start..batch_end {
                    all_embeddings.push(vec![0.0; config.dimensions]);
                }
            }
        }
    }

    // Pair embeddings back to patterns/workflows
    let mut indexed_patterns: Vec<(Pattern, Vec<f32>)> = Vec::new();
    let mut indexed_workflows: Vec<(Workflow, Vec<f32>)> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        if i < all_embeddings.len() && !all_embeddings[i].is_empty() && all_embeddings[i][0] != 0.0 || all_embeddings[i].len() > 1 {
            if item.is_workflow {
                indexed_workflows.push((workflows[item.index].clone(), all_embeddings[i].clone()));
            } else {
                indexed_patterns.push((patterns[item.index].clone(), all_embeddings[i].clone()));
            }
        }
    }
```

Wait — the zero-check logic is fragile. Let me use a simpler approach: track which indices succeeded.

Actually, let me simplify. Just collect into a Vec<Option<Vec<f32>>>:

```rust
    // Collect all texts
    let mut texts: Vec<String> = Vec::with_capacity(patterns.len() + workflows.len());

    for pattern in &patterns {
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
        texts.push(text);
    }
    for workflow in &workflows {
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
        texts.push(text);
    }

    let total = texts.len();
    let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; total];
    let mut errors = 0;

    const EMBED_BATCH: usize = 200;
    for batch_start in (0..total).step_by(EMBED_BATCH) {
        let batch_end = (batch_start + EMBED_BATCH).min(total);
        let batch: Vec<String> = texts[batch_start..batch_end].to_vec();
        match embed_batch(&batch, &config).await {
            Ok(batch_embs) => {
                for (j, emb) in batch_embs.into_iter().enumerate() {
                    embeddings[batch_start + j] = Some(emb);
                }
                println!("  {}/{} embedded...", batch_end, total);
            }
            Err(e) => {
                eprintln!("  ⚠️  batch {}-{} embedding failed: {}", batch_start, batch_end, e);
                errors += batch_end - batch_start;
            }
        }
    }

    // Pair results back
    let mut indexed_patterns = Vec::new();
    let mut indexed_workflows = Vec::new();

    for (i, pattern) in patterns.iter().enumerate() {
        if let Some(emb) = embeddings[i].take() {
            indexed_patterns.push((pattern.clone(), emb));
        }
    }
    for (i, workflow) in workflows.iter().enumerate() {
        if let Some(emb) = embeddings[patterns.len() + i].take() {
            indexed_workflows.push((workflow.clone(), emb));
        }
    }
```

Also remove the now-unused `use crate::store::embedding::{EmbeddingConfig, embed};` import — change to `embed_batch`:

Line 7: change `embed` to `embed_batch` in the import.

- [ ] **Step 2: Build check**

```bash
cargo build -p mur-core 2>&1 | head -20
```
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/reindex.rs
git commit -m "perf(reindex): use embed_batch instead of sequential per-item embed

cmd_reindex was calling embed() once per pattern/workflow in a loop.
With OpenAI-compatible providers this meant N sequential HTTP requests.
Now collects all texts and calls embed_batch() in chunks of 200."
```

---

### Task 3: Add `IndexProgress` and `IndexLock` types + lock file I/O

**Files:**
- Modify: `mur-core/src/codebase/mod.rs`

Add types for progress tracking and lock file. Reuse the existing `pid_alive()` from `mur_common::lock_file` for lock validation.

- [ ] **Step 1: Add new types after `DiscoveredIndex` (after line 67)**

```rust
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
```

- [ ] **Step 2: Add const and path helpers to `CodebaseIndex` impl block (after line 95)**

```rust
/// Chunks above this threshold auto-trigger background mode.
pub const BACKGROUND_CHUNK_THRESHOLD: usize = 200;

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
```

- [ ] **Step 3: Add lock file methods to `CodebaseIndex`**

```rust
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
```

- [ ] **Step 4: Build check**

```bash
cargo build -p mur-core 2>&1 | head -20
```
Expected: compiles. `IndexProgress`/`IndexLock` not yet used, so allow dead_code warnings temporarily.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/codebase/mod.rs
git commit -m "feat(index): add IndexProgress, IndexLock types and lock file I/O

Adds:
- IndexProgress: written during background indexing, read by `mur project status`
- IndexLock: lightweight PID lock to prevent concurrent indexing
- try_acquire_lock / release_lock / write_progress / read_progress methods
- BACKGROUND_CHUNK_THRESHOLD constant (200 chunks)"
```

---

### Task 4: Add background worker subcommand + CLI flags

**Files:**
- Modify: `mur-core/src/cli/mod.rs`
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cmd/project.rs`

Add `--background`/`--foreground` flags and a hidden `ProjectIndexWorker` subcommand that does the actual work when spawned.

- [ ] **Step 1: Add `BackgroundMode` to CLI args in `cli/mod.rs`**

Find the `ProjectAction::Index` variant. Add fields:

```rust
// In ProjectAction::Index:
ProjectAction::Index {
    path: Option<String>,
    rebuild: bool,
    quiet: bool,
    /// None = auto-detect, Some(true) = force background, Some(false) = force foreground
    background: Option<bool>,
}
```

Also add the hidden worker subcommand:
```rust
// In ProjectAction enum, add:
ProjectAction::IndexWorker {
    project_name: String,
    project_path: String,
    rebuild: bool,
}
```

Update the clap derive for `ProjectAction::Index`:
```rust
Index {
    /// Project path (defaults to current directory)
    path: Option<String>,
    /// Force full rebuild ignoring mtime cache
    #[arg(long)]
    rebuild: bool,
    /// Less output
    #[arg(long)]
    quiet: bool,
    /// Run indexing in background (default: auto-detect based on chunk count)
    #[arg(long, conflicts_with = "foreground")]
    background: bool,
    /// Force foreground execution even for large projects
    #[arg(long, conflicts_with = "background")]
    foreground: bool,
},
```

And the hidden worker:
```rust
/// Internal: spawned by `project index --background`. Not shown in help.
#[command(hide = true)]
IndexWorker {
    project_name: String,
    project_path: String,
    #[arg(long)]
    rebuild: bool,
},
```

- [ ] **Step 2: Update `dispatch.rs` to pass new flags**

Update the match arm for `ProjectAction::Index`:
```rust
ProjectAction::Index {
    path,
    rebuild,
    quiet,
    background,
} => {
    let mode = match (background, foreground) {
        (true, _) => BackgroundMode::ForceBackground,
        (_, true) => BackgroundMode::ForceForeground,
        (false, false) => BackgroundMode::Auto,
    };
    cmd::project::cmd_project_index(path, rebuild, quiet, mode).await?
}
```

Add dispatch for the worker:
```rust
ProjectAction::IndexWorker {
    project_name,
    project_path,
    rebuild,
} => cmd::project::cmd_project_index_worker(&project_name, &project_path, rebuild).await?,
```

- [ ] **Step 3: Add `BackgroundMode` enum and updated `cmd_project_index` in `cmd/project.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundMode {
    /// Auto-detect: background if chunks > BACKGROUND_CHUNK_THRESHOLD
    Auto,
    /// Force background execution
    ForceBackground,
    /// Force foreground execution
    ForceForeground,
}
```

Rewrite `cmd_project_index`:
```rust
pub(crate) async fn cmd_project_index(
    path: Option<String>,
    rebuild: bool,
    quiet: bool,
    bg_mode: BackgroundMode,
) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);

    if !project_path.join(".git").exists() {
        anyhow::bail!(
            "Not a git repository: {}\n  mur project index requires a git repo",
            project_path.display()
        );
    }

    let project_name = project_name_from_path(&project_path);
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index = CodebaseIndex::new(&project_name, &project_path);

    // Determine whether to run in background
    let run_in_background = match bg_mode {
        BackgroundMode::ForceBackground => true,
        BackgroundMode::ForceForeground => false,
        BackgroundMode::Auto => {
            // Quick scan to estimate chunk count
            let files = scan_project(&project_path);
            let estimated_chunks: usize = files.iter().map(|f| {
                let lines = f.content.lines().count();
                if lines < 50 { 1 } else { (lines / 60).max(1) + 1 }
            }).sum();
            estimated_chunks > BACKGROUND_CHUNK_THRESHOLD
        }
    };

    if run_in_background {
        // Check lock
        if !index.try_acquire_lock()? {
            eprintln!(
                "Indexing already in progress for '{}' (lock held by another process).",
                project_name
            );
            eprintln!("  Check status: mur project status");
            return Ok(());
        }

        // Spawn worker subprocess
        let mur_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("mur"));
        let mut cmd = std::process::Command::new(&mur_bin);
        cmd.args(["project", "index-worker", &project_name, &project_path.display().to_string()]);
        if rebuild {
            cmd.arg("--rebuild");
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.stdin(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // Detach from parent process group
        }

        let child = cmd.spawn().with_context(|| "spawning background index worker")?;
        let pid = child.id();

        if !quiet {
            eprintln!(
                "Indexing '{}' in background (PID: {}). {} chunks estimated.",
                project_name, pid, estimated_chunks
            );
            eprintln!("  Check progress: mur project status");
        }

        // Note: lock file already written by try_acquire_lock() with OUR pid.
        // But the worker has a different pid! Overwrite with the child's pid.
        let lock = IndexLock {
            pid,
            project_name: project_name.clone(),
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        let lock_path = index.lock_path();
        std::fs::write(&lock_path, serde_json::to_string(&lock)?)?;

        // Write initial progress
        index.write_progress(&IndexProgress {
            status: IndexStatus::Running,
            total_chunks: estimated_chunks,
            done_chunks: 0,
            errors: 0,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            error_message: None,
        })?;

        return Ok(());
    }

    // ── Foreground path (existing behavior) ──
    if !quiet {
        eprintln!("Indexing {} ({})...", project_name, project_path.display());
    }

    let stats = index
        .build(&embed_config, rebuild, |done, total| {
            if !quiet && total > 0 {
                eprint!("\r  Embedding {}/{} chunks...", done, total);
            }
        })
        .await?;

    if !quiet {
        eprintln!();
        eprintln!(
            "  {} files, {} chunks, {} changed, {} skipped ({:.1}s)",
            stats.files_indexed,
            stats.chunks_created,
            stats.files_changed,
            stats.files_skipped,
            stats.duration_ms as f64 / 1000.0
        );
    }

    ensure_git_hook(&project_path, quiet)?;
    Ok(())
}
```

- [ ] **Step 4: Add `cmd_project_index_worker` function**

```rust
/// Internal: runs the actual indexing work when spawned in background mode.
pub(crate) async fn cmd_project_index_worker(
    project_name: &str,
    project_path_str: &str,
    rebuild: bool,
) -> Result<()> {
    let project_path = PathBuf::from(project_path_str);
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index = CodebaseIndex::new(project_name, &project_path);

    // Build with progress callbacks that write to the progress file
    let build_result = index
        .build(&embed_config, rebuild, |done, total| {
            let progress = IndexProgress {
                status: IndexStatus::Running,
                total_chunks: total,
                done_chunks: done,
                errors: 0,
                started_at: String::new(), // not used for running updates
                finished_at: None,
                error_message: None,
            };
            let _ = index.write_progress(&progress);
        })
        .await;

    match build_result {
        Ok(stats) => {
            let progress = IndexProgress {
                status: IndexStatus::Done,
                total_chunks: stats.chunks_created,
                done_chunks: stats.chunks_created,
                errors: 0,
                started_at: String::new(),
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error_message: None,
            };
            let _ = index.write_progress(&progress);
            index.release_lock();

            // Desktop notification
            send_notification(
                &format!("mur: {} indexed", project_name),
                &format!(
                    "{} files, {} chunks in {:.1}s",
                    stats.files_indexed,
                    stats.chunks_created,
                    stats.duration_ms as f64 / 1000.0
                ),
            );
        }
        Err(e) => {
            let progress = IndexProgress {
                status: IndexStatus::Error,
                total_chunks: 0,
                done_chunks: 0,
                errors: 1,
                started_at: String::new(),
                finished_at: Some(chrono::Utc::now().to_rfc3339()),
                error_message: Some(e.to_string()),
            };
            let _ = index.write_progress(&progress);
            index.release_lock();

            send_notification(
                &format!("mur: {} index failed", project_name),
                &format!("Error: {:.100}", e),
            );
        }
    }

    Ok(())
}

/// Send a desktop notification. macOS uses osascript; Linux uses notify-send if available.
fn send_notification(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            message.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Fallback: terminal bell + stderr
        eprintln!("\n\x07{}: {}", title, message);
        // Try notify-send on Linux
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("notify-send")
                .args([title, message])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}
```

- [ ] **Step 5: Add imports to `cmd/project.rs`**

Add to existing imports:
```rust
use crate::codebase::{CodebaseIndex, IndexProgress, IndexStatus, IndexLock, BACKGROUND_CHUNK_THRESHOLD};
use std::path::PathBuf;
```

- [ ] **Step 6: Build check**

```bash
cargo build -p mur-core 2>&1
```
Expected: compiles. Fix any missing imports or type errors.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cli/mod.rs mur-core/src/dispatch.rs mur-core/src/cmd/project.rs
git commit -m "feat(index): add --background/--foreground flags with auto-detect

- BackgroundMode enum: Auto / ForceBackground / ForceForeground
- Auto-detect: estimate chunk count from file scan; if > 200, spawn worker
- Hidden `project index-worker` subcommand for the background process
- Worker writes progress to .progress.json and sends macOS notification on completion
- Lock file prevents concurrent indexing of the same project"
```

---

### Task 5: Update `mur project status` to show live background progress

**Files:**
- Modify: `mur-core/src/cmd/project.rs` (`cmd_project_status` function)

- [ ] **Step 1: Update `cmd_project_status` to read progress file**

Replace the existing `cmd_project_status` function:

```rust
pub(crate) async fn cmd_project_status(path: Option<String>) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    let has_db = index.lance_path().exists();
    let stats = index.stats_async().await?;

    println!("Project: {}", project_name);
    println!("  Path: {}", project_path.display());

    // Check if a background index is running
    let lock_path = index.lock_path();
    if lock_path.exists() {
        if let Ok(data) = std::fs::read_to_string(&lock_path) {
            if let Ok(lock) = serde_json::from_str::<IndexLock>(&data) {
                if mur_common::lock_file::pid_alive(lock.pid) {
                    // Live background process — show progress
                    if let Some(progress) = index.read_progress() {
                        let pct = if progress.total_chunks > 0 {
                            (progress.done_chunks as f64 / progress.total_chunks as f64) * 100.0
                        } else {
                            0.0
                        };
                        println!("  Status: indexing in background (PID: {})", lock.pid);
                        println!("  Progress: {}/{} chunks ({:.0}%)",
                            progress.done_chunks, progress.total_chunks, pct);
                        if progress.errors > 0 {
                            println!("  Errors: {}", progress.errors);
                        }
                    } else {
                        println!("  Status: indexing in background (PID: {})", lock.pid);
                    }
                    return Ok(());
                } else {
                    // Stale lock — process died
                    if let Some(progress) = index.read_progress() {
                        match progress.status {
                            IndexStatus::Done => {
                                println!("  Last index: completed");
                            }
                            IndexStatus::Error => {
                                println!("  Last index: failed");
                                if let Some(ref msg) = progress.error_message {
                                    println!("  Error: {}", msg);
                                }
                            }
                            IndexStatus::Running => {
                                println!("  Last index: interrupted (stale lock, PID {} no longer alive)", lock.pid);
                            }
                        }
                    } else {
                        println!("  Last index: unknown (stale lock)");
                    }
                }
            }
        }
    }

    println!("  Indexed: {}", if has_db { "yes" } else { "no" });
    if has_db {
        println!("  Chunks: {}", stats.chunks_created);
    }

    Ok(())
}
```

- [ ] **Step 2: Build check**

```bash
cargo build -p mur-core 2>&1
```
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/project.rs
git commit -m "feat(status): show live background indexing progress in `mur project status`

Reads .lock and .progress.json files to display:
- Live progress (% done, chunk count) when indexing is running
- Completion/error status for finished background runs
- Stale lock detection for crashed/interrupted runs"
```

---

### Task 6: Update `ensure_git_hook` to use `--background`

**Files:**
- Modify: `mur-core/src/codebase/mod.rs:594-631`

- [ ] **Step 1: Change hook to use `--background` flag**

Replace line 609:
```rust
// Before:
"{} project index \"{}\" --quiet &\n"

// After:
"{} project index \"{}\" --quiet --background\n"
```

The `--background` flag already handles: spawning a subprocess (so no `&` needed), lock file protection (prevents concurrent runs), and notification on completion. The `--quiet` suppresses the "Indexing in background..." message during git commit.

- [ ] **Step 2: Commit**

```bash
git add mur-core/src/codebase/mod.rs
git commit -m "fix(hook): use --background flag in git post-commit hook

--background provides lock file protection against concurrent indexing
and desktop notification on completion. No more bare `&` backgrounding."
```

---

### Task 7: End-to-end smoke test

- [ ] **Step 1: Build release binary**

```bash
cargo build --release -p mur-core 2>&1 | tail -5
```
Expected: builds successfully.

- [ ] **Step 2: Test foreground small project**

```bash
# Create a tiny test repo
cd /tmp
mkdir test-mur-index && cd test-mur-index
git init
echo 'fn main() { println!("hello"); }' > main.rs
git add main.rs && git commit -m "init"

# Run index (should be foreground — tiny project)
./target/release/mur project index --foreground
```
Expected: completes in foreground, shows progress, installs git hook.

- [ ] **Step 3: Test background mode (force)**

```bash
./target/release/mur project index --background
```
Expected: prints "Indexing 'test-mur-index' in background (PID: X)", returns immediately.

- [ ] **Step 4: Test `mur project status` with live progress**

```bash
./target/release/mur project status
```
Expected: shows "Status: indexing in background (PID: X)" with progress.

- [ ] **Step 5: Test lock file prevents concurrent runs**

```bash
./target/release/mur project index --background
# Should print "Indexing already in progress"
```
Expected: second run detects lock and refuses.

- [ ] **Step 6: Test background completion notification**

Wait for the background process to finish. macOS should show notification "mur: test-mur-index indexed".

- [ ] **Step 7: Test `mur project status` after completion**

```bash
./target/release/mur project status
```
Expected: shows "Indexed: yes" with chunk count.

- [ ] **Step 8: Run existing tests**

```bash
cargo test -p mur-core -- codebase 2>&1
cargo test -p mur-core -- store::embedding 2>&1
cargo test -p mur-core -- store::vector 2>&1
```
Expected: all pass.

- [ ] **Step 9: Full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 10: Commit any test fixes**

```bash
git add -A
git commit -m "test: add smoke test for project index background mode"
```

---

## Post-Implementation

After all tasks are done:

```bash
# Full verification
cargo build --release -p mur-core
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
