# MUR Project Codebase Index Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move codebase indexing from mur-commander into mur-core so both `mur` and `mur-commander` share one implementation, reusing mur's embedding pipeline and storing indexes under `~/.mur/indexes/codebase/`.

**Architecture:** Port commander's scanner + chunker + CodebaseIndex into a new `mur-core/src/codebase/` module. Adapt the indexer to use mur's `store::embedding` (multi-provider: Ollama + OpenAI) while keeping LanceDB directly for storage (code chunks have a fundamentally different schema than patterns/sources). Add `mur project {index,search,status,list}` CLI. Replace commander's `cmd_index` and `search_codebase_for_tool` with thin shell-outs to `mur project`. Commander drops its `codebase/` module and `memory/embed.rs`.

**Tech Stack:** Rust edition 2024, `lancedb` 0.26, `arrow-array/schema` 57, `regex`, `walkdir`, `serde_json`, `chrono`, `tokio`, `anyhow`

---

## File Structure

```
mur-core/src/codebase/
├── mod.rs            # CodebaseIndex, CodeChunk, IndexStats, discover_all, ensure_git_hook
├── scanner.rs        # scan_project, language_for_ext, walk_dir, gitignore support
└── chunker.rs        # chunk_file, boundary_regex, extract_symbol, sliding_window

mur-core/src/cmd/
└── project.rs        # CLI handlers: cmd_project_index, cmd_project_search, cmd_project_status

mur-core/src/cli/
└── mod.rs            # + Project subcommand enum + Commands::Project variant

mur-core/src/
├── dispatch.rs       # + Commands::Project arm
└── lib.rs            # + pub mod codebase;

mur-commander/crates/cli/src/
└── commands.rs       # Replace cmd_index impl with mur shell-out
└── daemon.rs         # Update git hook template (murc → mur)

mur-commander/crates/gateway/src/unified_handler/
└── dispatch.rs       # Replace search_codebase_for_tool with mur shell-out

mur-commander/crates/engine/src/
├── lib.rs            # Remove codebase module, memory::embed module
└── codebase/         # DELETE entire directory
└── memory/embed.rs   # DELETE
```

---

### Task 1: Create `codebase/scanner.rs` — file scanner ported from commander

**Files:**
- Create: `mur-core/src/codebase/scanner.rs`

**Purpose:** Port commander's scanner.rs with minimal changes. Reuses `walkdir` (already in mur's deps), `regex` (already in deps). Uses mur's `paths::mur_root()` instead of commander's `commander_dir()`.

- [ ] **Step 1: Create the module directory and scanner.rs**

```bash
mkdir -p mur-core/src/codebase
```

Write `mur-core/src/codebase/scanner.rs`:

```rust
//! File scanner for codebase indexing.
//!
//! Walks a project directory recursively, respecting `.gitignore` and
//! hardcoded skip patterns. Returns a list of files with their content
//! and detected language.

use std::fs;
use std::path::{Path, PathBuf};

/// A scanned source file ready for chunking.
pub struct ScannedFile {
    /// Path relative to the project root.
    pub relative_path: String,
    /// Full file content (UTF-8).
    pub content: String,
    /// Detected programming language.
    pub language: String,
}

/// Directories to always skip.
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "build", "dist", "__pycache__",
    ".venv", "vendor", ".next", ".nuxt", ".turbo", ".svelte-kit",
    "Pods", ".build", "DerivedData",
];

/// File extensions to index, mapped to language names.
fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "py" => Some("python"),
        "swift" => Some("swift"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "scala" => Some("scala"),
        "ex" | "exs" => Some("elixir"),
        "zig" => Some("zig"),
        "lua" => Some("lua"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "json" => Some("json"),
        "md" => Some("markdown"),
        "sql" => Some("sql"),
        "proto" => Some("protobuf"),
        "tf" => Some("terraform"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

/// Scan a project directory and return all indexable source files.
pub fn scan_project(project_path: &Path) -> Vec<ScannedFile> {
    let gitignore = load_gitignore(project_path);
    let mut files = Vec::new();
    walk_dir(project_path, project_path, &gitignore, &mut files);
    files
}

fn walk_dir(root: &Path, dir: &Path, gitignore_patterns: &[String], out: &mut Vec<ScannedFile>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            if SKIP_DIRS.iter().any(|&s| name == s) {
                continue;
            }
            let rel = relative_path(root, &path);
            if is_gitignored(&rel, gitignore_patterns, true) {
                continue;
            }
            walk_dir(root, &path, gitignore_patterns, out);
        } else if path.is_file() {
            let rel = relative_path(root, &path);
            if is_gitignored(&rel, gitignore_patterns, false) {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let language = match language_for_ext(ext) {
                Some(lang) => lang,
                None => continue,
            };

            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() > 512 * 1024 {
                continue;
            }

            let content = match fs::read(&path) {
                Ok(bytes) => {
                    let check_len = bytes.len().min(512);
                    if bytes[..check_len].contains(&0) {
                        continue;
                    }
                    match String::from_utf8(bytes) {
                        Ok(s) => s,
                        Err(_) => continue,
                    }
                }
                Err(_) => continue,
            };

            out.push(ScannedFile {
                relative_path: rel,
                content,
                language: language.to_string(),
            });
        }
    }
}

fn load_gitignore(project_path: &Path) -> Vec<String> {
    let gitignore_path = project_path.join(".gitignore");
    let content = match fs::read_to_string(gitignore_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.strip_prefix('/').unwrap_or(l).to_string())
        .map(|l| l.strip_suffix('/').unwrap_or(&l).to_string())
        .collect()
}

fn is_gitignored(rel_path: &str, patterns: &[String], is_dir: bool) -> bool {
    for pattern in patterns {
        if rel_path == *pattern || rel_path.starts_with(&format!("{pattern}/")) {
            return true;
        }
        if !pattern.contains('/') {
            let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
            if file_name == *pattern {
                return true;
            }
            if is_dir {
                for component in rel_path.split('/') {
                    if component == *pattern {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Get the project name from a path (last component).
pub fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Expand ~ in a path string to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(language_for_ext("rs"), Some("rust"));
        assert_eq!(language_for_ext("ts"), Some("typescript"));
        assert_eq!(language_for_ext("py"), Some("python"));
        assert_eq!(language_for_ext("xyz"), None);
    }

    #[test]
    fn test_gitignore_matching() {
        let patterns = vec!["dist".to_string(), "*.log".to_string()];
        assert!(is_gitignored("dist", &patterns, true));
        assert!(is_gitignored("dist/index.js", &patterns, false));
        assert!(!is_gitignored("src/main.rs", &patterns, false));
    }

    #[test]
    fn test_project_name() {
        let path = PathBuf::from("/home/user/Projects/my-app");
        assert_eq!(project_name_from_path(&path), "my-app");
    }
}
```

- [ ] **Step 2: Run scanner tests**

```bash
cargo test -p mur-core codebase::scanner
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/codebase/scanner.rs
git commit -m "feat(codebase): port file scanner from mur-commander"
```

---

### Task 2: Create `codebase/chunker.rs` — language-aware chunker ported from commander

**Files:**
- Create: `mur-core/src/codebase/chunker.rs`

**Purpose:** Port commander's chunker.rs verbatim. It's self-contained with no dependencies on either codebase's infrastructure — only uses `regex`. All 28 language-specific boundary regexes and the sliding-window fallback come over as-is.

- [ ] **Step 1: Write chunker.rs**

```rust
//! Code chunking for codebase indexing.
//!
//! Splits source files into semantically meaningful chunks based on
//! language-specific boundaries (functions, classes, structs, etc.).
//! Falls back to a sliding window for unsupported languages.

use regex::Regex;

/// A chunk of code extracted from a source file.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub chunk_type: String,
    pub symbol: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
}

const SMALL_FILE_LINES: usize = 50;
const WINDOW_SIZE: usize = 80;
const WINDOW_OVERLAP: usize = 20;

/// Split a file's content into chunks based on language.
pub fn chunk_file(content: &str, language: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();

    if lines.len() < SMALL_FILE_LINES {
        return vec![Chunk {
            chunk_type: "file".to_string(),
            symbol: None,
            content: content.to_string(),
            line_start: 1,
            line_end: lines.len() as u32,
        }];
    }

    let boundary_re = boundary_regex(language);
    match boundary_re {
        Some(re) => split_by_boundaries(&lines, &re, language),
        None => sliding_window(&lines),
    }
}

fn boundary_regex(language: &str) -> Option<Regex> {
    let pattern = match language {
        "rust" => r"^(pub\s+)?(async\s+)?(fn|impl|struct|enum|mod|trait)\s",
        "typescript" | "javascript" | "vue" | "svelte" => {
            r"^(export\s+)?(default\s+)?(async\s+)?(function|class|const|let|interface|type|enum)\s"
        }
        "go" => r"^(func|type)\s",
        "python" => r"^(async\s+)?(def|class)\s",
        "swift" => {
            r"^(\s*)(public\s+|private\s+|internal\s+|open\s+|fileprivate\s+)?(static\s+)?(func|class|struct|enum|protocol|extension)\s"
        }
        "java" | "kotlin" => {
            r"^(\s*)(public|private|protected|internal)?\s*(static\s+)?(class|interface|fun|func|enum|object)\s"
        }
        "ruby" => r"^(\s*)(def|class|module)\s",
        "php" => r"^(\s*)(public|private|protected)?\s*(static\s+)?(function|class|interface|trait)\s",
        "c" | "cpp" => r"^(\w[\w\s\*]*)\s+(\w+)\s*\(",
        "elixir" => r"^(\s*)(def|defp|defmodule)\s",
        _ => return None,
    };
    Regex::new(pattern).ok()
}

fn split_by_boundaries(lines: &[&str], re: &Regex, language: &str) -> Vec<Chunk> {
    let mut boundaries: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line) {
            boundaries.push(i);
        }
    }

    if boundaries.is_empty() {
        return sliding_window(lines);
    }

    let mut chunks = Vec::new();

    // Content before first boundary → "block" chunk
    if boundaries[0] > 0 {
        let content: String = lines[..boundaries[0]].join("\n");
        if content.trim().len() > 10 {
            chunks.push(Chunk {
                chunk_type: "block".to_string(),
                symbol: None,
                content,
                line_start: 1,
                line_end: boundaries[0] as u32,
            });
        }
    }

    for (idx, &start) in boundaries.iter().enumerate() {
        let end = if idx + 1 < boundaries.len() {
            boundaries[idx + 1]
        } else {
            lines.len()
        };

        let chunk_content: String = lines[start..end].join("\n");
        let symbol = extract_symbol(lines[start], language);
        let chunk_type = detect_chunk_type(lines[start], language);

        chunks.push(Chunk {
            chunk_type,
            symbol,
            content: chunk_content,
            line_start: (start + 1) as u32,
            line_end: end as u32,
        });
    }

    chunks
}

fn extract_symbol(line: &str, language: &str) -> Option<String> {
    let pattern = match language {
        "rust" => r"(?:pub\s+)?(?:async\s+)?(?:fn|impl|struct|enum|mod|trait)\s+(\w+)",
        "typescript" | "javascript" | "vue" | "svelte" => {
            r"(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:function|class|const|let|interface|type|enum)\s+(\w+)"
        }
        "go" => r"(?:func|type)\s+(?:\([^)]*\)\s+)?(\w+)",
        "python" => r"(?:async\s+)?(?:def|class)\s+(\w+)",
        "swift" => r"(?:func|class|struct|enum|protocol|extension)\s+(\w+)",
        "java" | "kotlin" => r"(?:class|interface|fun|func|enum|object)\s+(\w+)",
        "ruby" => r"(?:def|class|module)\s+(\w+)",
        "php" => r"(?:function|class|interface|trait)\s+(\w+)",
        "elixir" => r"(?:def|defp|defmodule)\s+(\w+)",
        _ => return None,
    };
    Regex::new(pattern)
        .ok()
        .and_then(|re| re.captures(line))
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn detect_chunk_type(line: &str, language: &str) -> String {
    let trimmed = line.trim();
    match language {
        "rust" => {
            if trimmed.contains("fn ") { "function" }
            else if trimmed.contains("impl ") { "impl" }
            else if trimmed.contains("struct ") { "struct" }
            else if trimmed.contains("enum ") { "enum" }
            else if trimmed.contains("trait ") { "trait" }
            else if trimmed.contains("mod ") { "module" }
            else { "block" }
        }
        "typescript" | "javascript" | "vue" | "svelte" => {
            if trimmed.contains("function ") || trimmed.contains("=> ") { "function" }
            else if trimmed.contains("class ") { "class" }
            else if trimmed.contains("interface ") || trimmed.contains("type ") { "type" }
            else { "block" }
        }
        "go" => {
            if trimmed.starts_with("func") { "function" } else { "type" }
        }
        "python" => {
            if trimmed.contains("class ") { "class" } else { "function" }
        }
        "swift" => {
            if trimmed.contains("func ") { "function" }
            else if trimmed.contains("class ") { "class" }
            else if trimmed.contains("struct ") { "struct" }
            else if trimmed.contains("enum ") { "enum" }
            else { "block" }
        }
        _ => {
            if trimmed.contains("class ") { "class" }
            else if trimmed.contains("func") || trimmed.contains("def ") { "function" }
            else { "block" }
        }
    }
    .to_string()
}

fn sliding_window(lines: &[&str]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < lines.len() {
        let end = (start + WINDOW_SIZE).min(lines.len());
        let content: String = lines[start..end].join("\n");
        chunks.push(Chunk {
            chunk_type: "block".to_string(),
            symbol: None,
            content,
            line_start: (start + 1) as u32,
            line_end: end as u32,
        });
        if end >= lines.len() {
            break;
        }
        start += WINDOW_SIZE - WINDOW_OVERLAP;
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_small_file_single_chunk() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let chunks = chunk_file(content, "rust");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, "file");
    }

    #[test]
    fn test_rust_function_boundaries() {
        let mut lines = Vec::new();
        lines.push("use std::io;");
        lines.push("");
        lines.push("pub fn foo() {");
        lines.extend(std::iter::repeat_n("    let x = 1;", 25));
        lines.push("}");
        lines.push("");
        lines.push("pub async fn bar() {");
        lines.extend(std::iter::repeat_n("    let y = 2;", 25));
        lines.push("}");

        let content = lines.join("\n");
        let chunks = chunk_file(&content, "rust");
        assert!(chunks.len() >= 2);
        let symbols: Vec<_> = chunks.iter().filter_map(|c| c.symbol.as_deref()).collect();
        assert!(symbols.contains(&"foo"));
        assert!(symbols.contains(&"bar"));
    }

    #[test]
    fn test_sliding_window() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let chunks = chunk_file(&content, "unknown_lang");
        assert!(chunks.len() > 1);
        assert_eq!(chunks[0].chunk_type, "block");
    }

    #[test]
    fn test_python_boundaries() {
        let mut lines = Vec::new();
        lines.push("import os");
        lines.push("");
        lines.push("class MyClass:");
        lines.extend(std::iter::repeat_n("    pass", 25));
        lines.push("");
        lines.push("def my_function():");
        lines.extend(std::iter::repeat_n("    pass", 25));

        let content = lines.join("\n");
        let chunks = chunk_file(&content, "python");
        let symbols: Vec<_> = chunks.iter().filter_map(|c| c.symbol.as_deref()).collect();
        assert!(symbols.contains(&"MyClass"));
        assert!(symbols.contains(&"my_function"));
    }
}
```

- [ ] **Step 2: Run chunker tests**

```bash
cargo test -p mur-core codebase::chunker
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/codebase/chunker.rs
git commit -m "feat(codebase): port language-aware chunker from mur-commander"
```

---

### Task 3: Create `codebase/mod.rs` — CodebaseIndex engine with mur's embedding

**Files:**
- Create: `mur-core/src/codebase/mod.rs`

**Purpose:** Port commander's CodebaseIndex, adapted to use mur's `store::embedding::embed_batch()` instead of commander's `OllamaEmbedder`. Stores indexes at `~/.mur/indexes/codebase/{project}.lance/` and metadata at `~/.mur/indexes/codebase/{project}.meta.json`. Uses LanceDB directly for storage (code chunk schema is richer than mur's generic VectorStore schema).

- [ ] **Step 1: Write mod.rs**

```rust
//! Codebase indexing engine.
//!
//! Scans project files, chunks them by language-specific boundaries, embeds
//! each chunk via mur's multi-provider embedding pipeline, and stores vectors
//! in a per-project LanceDB index at `~/.mur/indexes/codebase/`.
//!
//! Usage:
//! ```ignore
//! let idx = CodebaseIndex::new("my-project", Path::new("/path/to/project"));
//! let stats = idx.build(&embed_config, false, |done, total| {}).await?;
//! let results = idx.search(&query_vec, 10).await?;
//! ```

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

/// Table name inside each project's LanceDB.
const TABLE_NAME: &str = "chunks";

/// Batch size for embedding calls.
const EMBED_BATCH_SIZE: usize = 200;

/// A code chunk result from search or indexing.
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

/// Statistics from an indexing run.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub chunks_created: usize,
    pub duration_ms: u64,
    pub files_changed: usize,
    pub files_skipped: usize,
}

/// Per-file metadata for incremental indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileMeta {
    mtime: u64,
    size: u64,
}

/// Metadata file stored alongside the LanceDB index for incremental builds.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexMetadata {
    #[serde(default)]
    pub project_path: String,
    pub files: HashMap<String, FileMeta>,
    pub last_indexed: String,
}

/// Info about an indexed project discovered by scanning the indexes directory.
pub struct DiscoveredIndex {
    pub name: String,
    pub project_path: Option<String>,
    pub last_indexed: Option<String>,
    pub file_count: usize,
}

/// Per-project codebase index backed by LanceDB.
pub struct CodebaseIndex {
    lance_path: PathBuf,
    project_name: String,
    project_path: PathBuf,
    db: Arc<OnceCell<lancedb::Connection>>,
}

impl CodebaseIndex {
    /// Create a new index. The LanceDB file lives at
    /// `~/.mur/indexes/codebase/<project-name>.lance`.
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

    /// Build (or rebuild) the index for this project.
    ///
    /// Uses incremental indexing: only re-embeds files whose mtime or size
    /// changed since the last run.
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

        // 1. Load existing metadata for incremental indexing
        let old_meta = if rebuild { None } else { self.load_meta() };

        // 2. Scan files
        let files = scan_project(&self.project_path);
        let files_indexed = files.len();

        // 3. Determine which files changed
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

        // 4. Chunk ALL files
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

        // 5. Retrieve cached embeddings for unchanged files from existing LanceDB
        let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; chunks_created];
        let has_existing_db = self.lance_path.exists();

        if has_existing_db && !unchanged_files.is_empty() {
            if let Ok(db) = self.get_db().await {
                let table_names = db.table_names().execute().await.unwrap_or_default();
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
                                            cache.insert(
                                                (file, ls.value(i), le.value(i)),
                                                emb,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        for (i, chunk) in all_chunks.iter().enumerate() {
                            if unchanged_file_set.contains(chunk.file.as_str()) {
                                let key =
                                    (chunk.file.clone(), chunk.line_start, chunk.line_end);
                                if let Some(emb) = cache.get(&key) {
                                    embeddings[i] = Some(emb.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        // 6. Collect chunks that still need embedding
        let chunks_to_embed: Vec<usize> = embeddings
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none())
            .map(|(i, _)| i)
            .collect();

        let total_to_embed = chunks_to_embed.len();

        // 7. Embed needed chunks in batches using mur's embedding pipeline
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

        let final_embeddings: Vec<Vec<f32>> =
            embeddings.into_iter().map(|e| e.unwrap_or_default()).collect();

        if final_embeddings.iter().any(|e| e.is_empty()) {
            anyhow::bail!("Some chunks failed to get embeddings");
        }

        // 8. Store in LanceDB (full rebuild of table)
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
        let vector_array =
            FixedSizeListArray::try_new(field, dim, Arc::new(values_array), None)?;

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

        // 9. Save updated metadata
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

    /// Search the index for chunks similar to a query embedding.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<CodeChunk>> {
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

    /// Get stats about the current index (async — counts rows in LanceDB).
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

    // ── Getters ─────────────────────────────────────────────────────

    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub fn lance_path(&self) -> &Path {
        &self.lance_path
    }
}

// ── Discovery ──────────────────────────────────────────────────────

/// Scan the indexes directory for all `.lance` directories and return info about each.
pub fn discover_all_indexes() -> Vec<DiscoveredIndex> {
    let indexes_dir = crate::paths::mur_root(None).join("indexes").join("codebase");
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

// ── Git Hook ───────────────────────────────────────────────────────

/// Ensure a git post-commit hook exists for auto-reindexing.
///
/// Returns `Ok(true)` if a new hook was installed, `Ok(false)` if it already existed.
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
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&hook_path)?;
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

// ── Schema ─────────────────────────────────────────────────────────

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
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim,
            ),
            false,
        ),
    ]))
}
```

- [ ] **Step 2: Compile to verify the module compiles**

```bash
cargo build -p mur-core 2>&1 | head -40
```

Expected: module compiles. Fix any import issues.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/codebase/mod.rs
git commit -m "feat(codebase): CodebaseIndex engine using mur's embedding pipeline"
```

---

### Task 4: Improve `embed_batch` to support true batch embedding

**Files:**
- Modify: `mur-core/src/store/embedding.rs`

**Purpose:** mur's current `embed_batch` sends one request per text, which is too slow for codebase indexing (thousands of chunks). Ollama's `/api/embed` accepts an array of strings. This change adds true batch support for the Ollama provider while keeping the sequential fallback for OpenAI.

- [ ] **Step 1: Add Ollama batch request support**

Read the current `embed_batch` and `embed_ollama` functions, then replace with batch-aware versions.

In `mur-core/src/store/embedding.rs`, replace the `embed_batch` function:

```rust
/// Batch embed multiple texts. Uses native batch APIs when available
/// (Ollama supports array input), falls back to sequential for OpenAI.
pub async fn embed_batch(texts: &[String], config: &EmbeddingConfig) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => {
            embed_ollama_batch(texts, base_url, &config.model).await
        }
        EmbeddingProvider::OpenAI { .. } => {
            // Sequential fallback for OpenAI (no native batch endpoint)
            let mut results = Vec::with_capacity(texts.len());
            for text in texts {
                results.push(embed(text, config).await?);
            }
            Ok(results)
        }
    }
}
```

Add the new `embed_ollama_batch` function after the existing `embed_ollama`:

```rust
async fn embed_ollama_batch(texts: &[String], base_url: &str, model: &str) -> Result<Vec<Vec<f32>>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&serde_json::json!({
            "model": model,
            "input": texts,
        }))
        .send()
        .await
        .context("calling Ollama batch embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error {}: {}", status, body);
    }

    let data: OllamaEmbedResponse = resp.json().await.context("parsing Ollama batch response")?;
    Ok(data.embeddings)
}
```

- [ ] **Step 2: Run existing embedding tests to verify no regression**

```bash
cargo test -p mur-core store::embedding
```

Expected: all existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/store/embedding.rs
git commit -m "perf(embedding): add true batch embedding for Ollama provider"
```

---

### Task 5: Register `codebase` module in lib.rs

**Files:**
- Modify: `mur-core/src/lib.rs`

- [ ] **Step 1: Add `pub mod codebase;` to lib.rs**

In `mur-core/src/lib.rs`, add after the existing `pub mod bridge_keychain;` line:

```rust
pub mod codebase;
```

- [ ] **Step 2: Verify compilation**

```bash
cargo build -p mur-core 2>&1 | head -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/lib.rs
git commit -m "feat(codebase): register codebase module in lib.rs"
```

---

### Task 6: Add `Project` CLI subcommand and clap types

**Files:**
- Modify: `mur-core/src/cli/mod.rs` — add `Project` variant + `ProjectAction` enum

- [ ] **Step 1: Add ProjectAction enum and Commands::Project variant**

In `mur-core/src/cli/mod.rs`, add the `ProjectAction` enum before the `Commands` enum:

```rust
/// Subcommands for `mur project`.
#[derive(Subcommand)]
pub enum ProjectAction {
    /// Index a project's source code for semantic search
    Index {
        /// Path to the project directory (defaults to current directory)
        #[arg(long)]
        path: Option<String>,
        /// Force full rebuild (ignore incremental cache)
        #[arg(long)]
        rebuild: bool,
        /// Suppress progress output
        #[arg(long)]
        quiet: bool,
    },
    /// Search indexed code for a query
    Search {
        /// Natural-language query to search code for
        query: String,
        /// Filter to a specific project name
        #[arg(long)]
        project: Option<String>,
        /// Max results to return
        #[arg(long, default_value = "5")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show indexing status for a project
    Status {
        /// Path to the project directory (defaults to current directory)
        #[arg(long)]
        path: Option<String>,
    },
    /// List all indexed projects
    List,
}
```

In the `Commands` enum, add the `Project` variant (e.g., after `Pattern`):

```rust
    /// Index and search project source code
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
```

- [ ] **Step 2: Compile to verify clap derives work**

```bash
cargo build -p mur-core 2>&1 | head -20
```

Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cli/mod.rs
git commit -m "feat(cli): add 'mur project' subcommand with index/search/status/list"
```

---

### Task 7: Implement CLI command handlers in `cmd/project.rs`

**Files:**
- Create: `mur-core/src/cmd/project.rs`

- [ ] **Step 1: Write cmd/project.rs with all four command handlers**

```rust
//! Command handlers for `mur project {index,search,status,list}`.

use anyhow::Result;
use std::path::PathBuf;

use crate::codebase::scanner::{expand_tilde, project_name_from_path};
use crate::codebase::{CodebaseIndex, discover_all_indexes};
use crate::store::config::load_config;
use crate::store::embedding::{EmbeddingConfig, embed};

pub(crate) async fn cmd_project_index(
    path: Option<String>,
    rebuild: bool,
    quiet: bool,
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

    if !quiet {
        eprintln!(
            "Indexing {} ({})...",
            project_name,
            project_path.display()
        );
    }

    let stats = index
        .build(&embed_config, rebuild, |done, total| {
            if !quiet && total > 0 {
                eprint!("\r  Embedding {}/{} chunks...", done, total);
            }
        })
        .await?;

    if !quiet {
        eprintln!(); // newline after progress
        eprintln!(
            "  {} files, {} chunks, {} changed, {} skipped ({:.1}s)",
            stats.files_indexed,
            stats.chunks_created,
            stats.files_changed,
            stats.files_skipped,
            stats.duration_ms as f64 / 1000.0
        );
    }

    // Install git hook for auto-reindex on commit
    crate::codebase::ensure_git_hook(&project_path, quiet)?;

    Ok(())
}

pub(crate) async fn cmd_project_search(
    query: String,
    project_filter: Option<String>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let cfg = load_config()?;
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let query_embedding = embed(&query, &embed_config).await?;

    let indexes = discover_all_indexes();
    if indexes.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No indexed projects found. Run `mur project index` first.");
        }
        return Ok(());
    }

    let mut all_results: Vec<serde_json::Value> = Vec::new();

    for discovered in &indexes {
        if let Some(ref filter) = project_filter {
            if discovered.name != *filter {
                continue;
            }
        }

        let project_path = discovered
            .project_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_default();
        let index = CodebaseIndex::new(&discovered.name, &project_path);
        let chunks = index.search(&query_embedding, limit).await?;

        for c in &chunks {
            all_results.push(serde_json::json!({
                "project": discovered.name,
                "file": c.file,
                "language": c.language,
                "chunk_type": c.chunk_type,
                "symbol": c.symbol,
                "content": c.content,
                "line_start": c.line_start,
                "line_end": c.line_end,
                "score": c.score,
            }));
        }
    }

    // Sort by score descending
    all_results.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(limit);

    if json {
        println!("{}", serde_json::to_string_pretty(&all_results)?);
    } else {
        for (i, result) in all_results.iter().enumerate() {
            let file = result["file"].as_str().unwrap_or("");
            let project = result["project"].as_str().unwrap_or("");
            let symbol = result["symbol"].as_str().unwrap_or("");
            let score = result["score"].as_f64().unwrap_or(0.0);
            let content = result["content"].as_str().unwrap_or("");
            let line_start = result["line_start"].as_u64().unwrap_or(0);
            let line_end = result["line_end"].as_u64().unwrap_or(0);

            println!(
                "{}. {}:{} ({}) lines {}-{} score={:.3}",
                i + 1, project, file, symbol, line_start, line_end, score
            );
            // Print first 3 lines of content indented
            for line in content.lines().take(3) {
                println!("   {}", line);
            }
            println!();
        }
    }

    Ok(())
}

pub(crate) async fn cmd_project_status(path: Option<String>) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    let meta = index.lance_path().exists();
    let stats = index.stats_async().await?;

    println!("Project: {}", project_name);
    println!("  Path: {}", project_path.display());
    println!("  Indexed: {}", if meta { "yes" } else { "no" });
    if meta {
        println!("  Chunks: {}", stats.chunks_created);
    }

    Ok(())
}

pub(crate) fn cmd_project_list() -> Result<()> {
    let indexes = discover_all_indexes();
    if indexes.is_empty() {
        println!("No indexed projects.");
        return Ok(());
    }
    println!("Indexed projects:");
    for idx in &indexes {
        let path_display = idx
            .project_path
            .as_deref()
            .unwrap_or("(unknown)");
        let last = idx
            .last_indexed
            .as_deref()
            .unwrap_or("(unknown)");
        println!(
            "  {} — {} files, last indexed: {}",
            idx.name, idx.file_count, last
        );
        println!("    path: {}", path_display);
    }
    Ok(())
}
```

- [ ] **Step 2: Register module in cmd/mod.rs**

In `mur-core/src/cmd/mod.rs`, add after the existing `pub(crate) mod pattern_history;` line:

```rust
pub(crate) mod project;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo build -p mur-core 2>&1 | head -20
```

Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/project.rs mur-core/src/cmd/mod.rs
git commit -m "feat(cli): implement 'mur project' command handlers"
```

---

### Task 8: Wire `Project` dispatch into `dispatch.rs`

**Files:**
- Modify: `mur-core/src/dispatch.rs`

- [ ] **Step 1: Add Project dispatch arm**

In `mur-core/src/dispatch.rs`, add the import for `ProjectAction` at the top (in the `use crate::cli::` block):

```rust
    ProjectAction,
```

Add the dispatch arm in `run()` (e.g., after the `Commands::Pattern` arm):

```rust
        Commands::Project { action } => match action {
            ProjectAction::Index { path, rebuild, quiet } => {
                cmd::project::cmd_project_index(path, rebuild, quiet).await?
            }
            ProjectAction::Search {
                query,
                project,
                limit,
                json,
            } => cmd::project::cmd_project_search(query, project, limit, json).await?,
            ProjectAction::Status { path } => cmd::project::cmd_project_status(path).await?,
            ProjectAction::List => cmd::project::cmd_project_list()?,
        },
```

- [ ] **Step 2: Verify compilation**

```bash
cargo build -p mur-core 2>&1 | head -20
```

Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/dispatch.rs
git commit -m "feat(cli): wire 'mur project' dispatch"
```

---

### Task 9: Smoke test `mur project index`

**Files:**
- None (test only)

- [ ] **Step 1: Build the mur binary**

```bash
cargo build --release
```

- [ ] **Step 2: Test indexing on the mur repo itself**

```bash
./target/release/mur project index --path .
```

Expected: scans files, embeds chunks via Ollama, prints stats, installs git hook.

- [ ] **Step 3: Test status**

```bash
./target/release/mur project status --path .
```

Expected: shows project name, indexed: yes, chunk count > 0.

- [ ] **Step 4: Test list**

```bash
./target/release/mur project list
```

Expected: shows "mur" in the list.

- [ ] **Step 5: Test search**

```bash
./target/release/mur project search "how does pattern storage work" --limit 3
```

Expected: returns relevant code chunks from the repo.

- [ ] **Step 6: Test JSON output**

```bash
./target/release/mur project search "pattern store" --limit 1 --json | head -20
```

Expected: valid JSON array output.

- [ ] **Step 7: Commit any fixes needed**

```bash
git add -A && git commit -m "fix(cli): address issues found during smoke test"
```

---

### Task 10: Update mur-commander — replace `cmd_index` with mur shell-out

**Files:**
- Modify: `~/Projects/mur-commander/crates/cli/src/commands.rs`

**Purpose:** Replace commander's in-process indexing with a thin shell-out to `mur project index`. This is a small change. Find the `cmd_index` function and replace its body.

- [ ] **Step 1: Replace cmd_index body**

In `~/Projects/mur-commander/crates/cli/src/commands.rs`, find `cmd_index` (around line 1590) and replace:

```rust
// cmd_index now delegates to mur project index
let mur_bin = which::which("mur")
    .unwrap_or_else(|_| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".mur")
            .join("bin")
            .join("mur")
    });
let mut cmd = std::process::Command::new(&mur_bin);
cmd.arg("project").arg("index");
if let Some(p) = &path {
    cmd.arg("--path").arg(p);
}
if rebuild {
    cmd.arg("--rebuild");
}
if quiet {
    cmd.arg("--quiet");
}
let status = cmd.status()?;
if !status.success() {
    anyhow::bail!("mur project index exited with {}", status);
}
// If this is the first index for the project, also add to config
// for search_codebase tool discovery
if let Some(p) = &path {
    auto_add_to_config(p)?;
}
Ok(())
```

- [ ] **Step 2: Update git hook template in daemon.rs**

In `~/Projects/mur-commander/crates/cli/src/daemon.rs`, find `install_git_hooks` and update the hook content to call `mur project index` instead of `murc index`:

Find the hook template (search for `mur-commander auto-index`) and replace `murc index` with `mur project index`.

- [ ] **Step 3: Build commander to verify**

```bash
cd ~/Projects/mur-commander && cargo build -p cli 2>&1 | head -20
```

- [ ] **Step 4: Commit commander changes**

```bash
cd ~/Projects/mur-commander
git add crates/cli/src/commands.rs crates/cli/src/daemon.rs
git commit -m "refactor(index): delegate 'murc index' to 'mur project index'"
```

---

### Task 11: Update mur-commander gateway — replace `search_codebase_for_tool` with mur shell-out

**Files:**
- Modify: `~/Projects/mur-commander/crates/gateway/src/unified_handler/dispatch.rs`

**Purpose:** Replace the inline LanceDB search with a shell-out to `mur project search --json`.

- [ ] **Step 1: Replace search_codebase_for_tool body**

Find `search_codebase_for_tool` (around line 2617), replace its implementation:

```rust
async fn search_codebase_for_tool(query: &str, project: Option<&str>) -> Result<String> {
    let mur_bin = which::which("mur").unwrap_or_else(|_| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".mur").join("bin").join("mur")
    });

    let mut cmd = tokio::process::Command::new(&mur_bin);
    cmd.arg("project").arg("search").arg(query).arg("--json").arg("--limit").arg("5");
    if let Some(p) = project {
        cmd.arg("--project").arg(p);
    }

    let output = cmd.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(format!("Search failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse JSON and format for LLM consumption
    let results: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap_or_default();
    if results.is_empty() {
        return Ok("No code results found.".into());
    }

    let mut formatted = String::from("## Codebase Search Results\n\n");
    for (i, r) in results.iter().enumerate() {
        let file = r["file"].as_str().unwrap_or("");
        let proj = r["project"].as_str().unwrap_or("");
        let sym = r["symbol"].as_str().unwrap_or("");
        let score = r["score"].as_f64().unwrap_or(0.0);
        let content = r["content"].as_str().unwrap_or("");

        formatted.push_str(&format!(
            "### {}. {}:{} ({}) — score: {:.3}\n```\n{}\n```\n\n",
            i + 1, proj, file, sym, score, content
        ));
    }
    Ok(formatted)
}
```

- [ ] **Step 2: Build gateway to verify**

```bash
cd ~/Projects/mur-commander && cargo build -p gateway 2>&1 | head -20
```

- [ ] **Step 3: Commit**

```bash
cd ~/Projects/mur-commander
git add crates/gateway/src/unified_handler/dispatch.rs
git commit -m "refactor(search): delegate codebase search to 'mur project search'"
```

---

### Task 12: Cleanup — remove dead code from mur-commander

**Files:**
- Delete: `~/Projects/mur-commander/crates/engine/src/codebase/` (entire directory)
- Delete: `~/Projects/mur-commander/crates/engine/src/memory/embed.rs`
- Modify: `~/Projects/mur-commander/crates/engine/src/lib.rs` — remove `pub mod codebase;` and related exports
- Modify: `~/Projects/mur-commander/crates/engine/Cargo.toml` — remove `lancedb`, `arrow-array`, `arrow-schema` dependencies if they were only used by codebase indexing

- [ ] **Step 1: Check if lancedb/arrow deps are still needed elsewhere in commander**

```bash
cd ~/Projects/mur-commander && rg "lancedb|arrow_array|arrow_schema" --type rust | grep -v codebase | grep -v embed
```

If no results, they can be removed from Cargo.toml.

- [ ] **Step 2: Remove dead code and dependencies**

```bash
cd ~/Projects/mur-commander
rm -rf crates/engine/src/codebase/
rm crates/engine/src/memory/embed.rs
```

Edit `crates/engine/src/lib.rs` — remove lines referencing `codebase` and `memory::embed`.

Edit `crates/engine/Cargo.toml` — remove unused deps (if any).

- [ ] **Step 3: Build to verify nothing is broken**

```bash
cd ~/Projects/mur-commander && cargo build 2>&1 | head -30
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
cd ~/Projects/mur-commander
git add -A
git commit -m "chore: remove codebase indexing engine (delegated to mur)"
```

---

### Task 13: Run full test suite and clippy on mur

**Files:**
- None

- [ ] **Step 1: Run all mur tests**

```bash
cargo test --workspace 2>&1
```

Expected: all tests pass.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings 2>&1
```

Expected: no warnings.

- [ ] **Step 3: Run rustfmt**

```bash
cargo fmt --check
```

Expected: no formatting issues. Fix any with `cargo fmt`.

---

### Task 14: Final integration test

**Files:**
- None

- [ ] **Step 1: Full end-to-end test of the mur workflow**

```bash
# Rebuild from scratch
./target/release/mur project index --path . --rebuild

# Verify status
./target/release/mur project status --path .

# Verify list
./target/release/mur project list

# Verify search
./target/release/mur project search "error handling in Rust" --limit 3

# Verify JSON output for machine consumption
./target/release/mur project search "vector store" --limit 2 --json
```

- [ ] **Step 2: Test commander integration**

```bash
# In mur-commander repo
cd ~/Projects/mur-commander
cargo run -- index --path /path/to/some/project
cargo run -- index --status
```

- [ ] **Step 3: Verify git hook was installed**

```bash
cat .git/hooks/post-commit
# Should contain "# mur auto-index" and "mur project index"
```

- [ ] **Step 4: Commit any final fixes**

```bash
git add -A && git commit -m "chore: integration test fixes"
```
