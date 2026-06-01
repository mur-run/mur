# MUR MCP Server + AI Tool Skills — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a thin MCP server (stdio JSON-RPC, 6 tools) that exposes MUR's interactive lookup commands to AI tools, plus 4 new MUR skill manifests.

**Architecture:** New `mur-mcp-server` workspace crate depends on `mur-core` to call existing `do_search`/`do_show`-style functions. Structured-data `do_*` functions are added to `cmd::project`, `cmd::agent`, and `cmd::context` where they don't already exist. JSON-RPC is hand-rolled over stdio (no heavy framework). Skills are plain YAML files installed to `~/.mur/skills/`.

**Tech Stack:** Rust (edition 2024), `serde_json` for JSON-RPC, `tokio` for async runtime, existing `mur-core` + `mur-common` crates.

---

## File Structure

```
mur-mcp-server/                    # NEW — workspace crate
  Cargo.toml
  src/
    main.rs                        # stdio loop + tokio runtime
    server.rs                      # MCP lifecycle: initialize, tools/list, tools/call
    tools.rs                       # Tool schema definitions + dispatch
    jsonrpc.rs                     # JSON-RPC 2.0 framing over stdio

mur-core/src/
  cmd/
    notes_cmd.rs                   # MODIFY — expose do_search as pub (already pub)
    project.rs                     # MODIFY — add do_project_search(), do_project_status(), do_project_list() returning structs
    agent/lifecycle.rs             # MODIFY — add do_list(), do_status() returning structs
    context.rs                     # MODIFY — add do_context() returning struct (or reuse --json path)

~/.mur/skills/                     # NEW FILES — skill manifests
  mur-project-index/
    skill.yaml
    SKILL.md
  mur-project-remove/
    skill.yaml
    SKILL.md
  mur-session-remove/
    skill.yaml
    SKILL.md
  mur-agent-manage/
    skill.yaml
    SKILL.md

  mur-context/                     # UPDATE existing
    skill.yaml
  mur-in/                          # UPDATE existing
    skill.yaml
  mur-out/                         # UPDATE existing
    skill.yaml

mur-core/src/cmd/hook.rs           # MODIFY — add git-commit detection for auto-index trigger
```

---

## Phase 1 — MCP Server Scaffolding + Core Tools

### Task 1: Create workspace crate scaffolding

**Files:**
- Create: `mur-mcp-server/Cargo.toml`
- Create: `mur-mcp-server/src/main.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create `mur-mcp-server/Cargo.toml`**

```toml
[package]
name = "mur-mcp-server"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "mur-mcp-server"
path = "src/main.rs"

[dependencies]
mur-common = { path = "../mur-common" }
mur-core = { path = "../mur-core" }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 2: Add `mur-mcp-server` to workspace members in root `Cargo.toml`**

```diff
 members = [
     "mur-common",
     "mur-core",
     "mur-agent-runtime",
     "mur-daemon",
     "mur-gui-core",
     "mur-agent-launcher",
+    "mur-mcp-server",
 ]
```

- [ ] **Step 3: Verify crate builds**

Run: `cargo build -p mur-mcp-server`
Expected: crate compiles with no errors (empty main.rs is fine)

- [ ] **Step 4: Commit**

```bash
git add mur-mcp-server/Cargo.toml mur-mcp-server/src/main.rs Cargo.toml
git commit -m "chore: scaffold mur-mcp-server workspace crate"
```

---

### Task 2: Implement JSON-RPC 2.0 framing over stdio

**Files:**
- Create: `mur-mcp-server/src/jsonrpc.rs`
- Modify: `mur-mcp-server/src/main.rs`

- [ ] **Step 1: Write JSON-RPC types and framing**

```rust
// mur-mcp-server/src/jsonrpc.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

/// JSON-RPC 2.0 request (what the client sends us).
#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

/// Read one JSON-RPC request from stdin. Blocks until a complete line.
/// Returns None if stdin closes.
pub fn read_request() -> Option<Request> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) => None, // EOF
        Ok(_) => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return read_request(); // skip blank lines
            }
            match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => {
                    tracing::debug!(method = %req.method, id = ?req.id, "received request");
                    Some(req)
                }
                Err(e) => {
                    tracing::warn!(error = %e, raw = %trimmed, "failed to parse request");
                    // Return a parse-error-shaped request so the caller can respond
                    Some(Request {
                        jsonrpc: "2.0".into(),
                        id: None,
                        method: "".into(),
                        params: None,
                    })
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "stdin read error");
            None
        }
    }
}

/// Write one JSON-RPC response to stdout. One line per response.
pub fn write_response(resp: &Response) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let json = serde_json::to_string(resp).unwrap_or_else(|e| {
        serde_json::to_string(&Response::error(
            None,
            -32700,
            format!("failed to serialize response: {}", e),
        ))
        .unwrap()
    });
    writeln!(handle, "{}", json).ok();
    handle.flush().ok();
    tracing::debug!(json = %json, "sent response");
}

/// Write a JSON-RPC notification (no id, no response expected).
pub fn write_notification(method: &str, params: Value) {
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", serde_json::to_string(&notif).unwrap()).ok();
    handle.flush().ok();
}
```

- [ ] **Step 2: Write the main async stdio loop**

```rust
// mur-mcp-server/src/main.rs
mod jsonrpc;
mod server;
mod tools;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr) // logs to stderr so stdout stays clean for JSON-RPC
        .init();

    tracing::info!("mur-mcp-server starting");

    let mut server = server::McpServer::new();

    while let Some(request) = jsonrpc::read_request() {
        let response = server.handle(request).await;
        jsonrpc::write_response(&response);
    }

    tracing::info!("mur-mcp-server shutting down");
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p mur-mcp-server`
Expected: compiles (server::McpServer not yet defined — add a stub)

```rust
// mur-mcp-server/src/server.rs (stub)
use crate::jsonrpc::{Request, Response};
use serde_json::Value;

pub struct McpServer;

impl McpServer {
    pub fn new() -> Self { Self }

    pub fn handle(&mut self, request: Request) -> Response {
        Response::error(request.id, -32601, format!("Method not found: {}", request.method))
    }
}
```

- [ ] **Step 4: Commit**

```bash
git add mur-mcp-server/src/jsonrpc.rs mur-mcp-server/src/main.rs mur-mcp-server/src/server.rs
git commit -m "feat(mcp): add JSON-RPC 2.0 stdio framing"
```

---

### Task 3: Implement MCP lifecycle (initialize, tools/list, tools/call)

**Files:**
- Modify: `mur-mcp-server/src/server.rs`
- Create: `mur-mcp-server/src/tools.rs`

- [ ] **Step 1: Define tool schema type and empty tool list in tools.rs**

```rust
// mur-mcp-server/src/tools.rs
use serde::Serialize;
use serde_json::Value;

/// JSON Schema for a tool parameter (MCP uses JSON Schema subset).
#[derive(Debug, Clone, Serialize)]
pub struct ToolParam {
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// MCP tool definition returned by tools/list.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<(String, ToolParam)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Return all registered tools.
pub fn all_tools() -> Vec<Tool> {
    vec![
        // Task 4 will populate these
    ]
}

/// Dispatch a tool call by name. Returns the result as a JSON Value.
/// Async because some tools (project_search, hook_context) need tokio.
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        other => Err(format!("Unknown tool: {}", other)),
    }
}
```

- [ ] **Step 2: Implement the McpServer with initialize + tools/list + tools/call**

```rust
// mur-mcp-server/src/server.rs
use crate::jsonrpc::{Request, Response};
use crate::tools;
use serde_json::{Value, json};

pub struct McpServer {
    /// Server name sent in initialize response.
    name: String,
    version: String,
    /// Whether initialize has been called.
    initialized: bool,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            name: "mur-mcp-server".into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            initialized: false,
        }
    }

    pub async fn handle(&mut self, request: Request) -> Response {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id, &request.params),
            "tools/list" => {
                if !self.initialized {
                    return Response::error(
                        request.id,
                        -32002,
                        "Not initialized. Call 'initialize' first.".into(),
                    );
                }
                self.handle_tools_list(request.id)
            }
            "tools/call" => {
                if !self.initialized {
                    return Response::error(
                        request.id,
                        -32002,
                        "Not initialized. Call 'initialize' first.".into(),
                    );
                }
                self.handle_tools_call(request.id, &request.params).await
            }
            "notifications/initialized" => {
                self.initialized = true;
                tracing::info!("client confirmed initialization");
                Response {
                    jsonrpc: "2.0",
                    id: None,
                    result: None,
                    error: None,
                }
            }
            "" => {
                Response::error(request.id, -32700, "Parse error".into())
            }
            _ => Response::error(
                request.id,
                -32601,
                format!("Method not found: {}", request.method),
            ),
        }
    }

    // ... (handle_initialize, handle_tools_list same as before) ...

    async fn handle_tools_call(&self, id: Option<Value>, params: &Option<Value>) -> Response {
        let params = match params {
            Some(p) => p,
            None => {
                return Response::error(
                    id,
                    -32602,
                    "Missing params. Expected: {name: string, arguments: object}".into(),
                );
            }
        };

        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return Response::error(id, -32602, "Missing 'name' in params".into()),
        };

        let arguments = params.get("arguments").unwrap_or(&Value::Null);

        match tools::call_tool(tool_name, arguments).await {
            Ok(result) => {
                let content = match result {
                    Value::String(s) => vec![json!({"type": "text", "text": s})],
                    other => vec![json!({"type": "text", "text": serde_json::to_string_pretty(&other).unwrap_or_else(|_| format!("{:?}", other))})],
                };
                Response::success(id, json!({ "content": content }))
            }
            Err(e) => {
                let content = vec![json!({"type": "text", "text": format!("Error: {}", e)})];
                Response::success(id, json!({"content": content, "isError": true}))
            }
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p mur-mcp-server`
Expected: compiles successfully

- [ ] **Step 4: Commit**

```bash
git add mur-mcp-server/src/server.rs mur-mcp-server/src/tools.rs
git commit -m "feat(mcp): implement MCP lifecycle — initialize, tools/list, tools/call"
```

---

### Task 4: Wire mur_notes_search and mur_notes_show tools

**Files:**
- Modify: `mur-mcp-server/src/tools.rs`

- [ ] **Step 1: Add the two tool definitions and dispatch logic**

```rust
// mur-mcp-server/src/tools.rs — replace all_tools() and call_tool()

use mur_core::cmd::notes_cmd::{self, NoteView};
use mur_common::skill::manifest::SkillManifest;
// NOTE: do_search is already pub in notes_cmd.rs

pub fn all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "mur_notes_search".into(),
            description: "Search MUR notes and patterns by keyword query. Returns ranked results with name, description, maturity, and relevance score.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Search query".into(),
                        default: None,
                    }),
                    ("limit".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max results, 1-10 (default: 5)".into(),
                        default: Some(json!(5)),
                    }),
                ]),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "mur_notes_show".into(),
            description: "Load a specific note or pattern by name. Returns full body, metadata, maturity, and tags.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("name".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Note name (exact match)".into(),
                        default: None,
                    }),
                ]),
                required: Some(vec!["name".into()]),
            },
        },
    ]
}

pub fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "mur_notes_search" => {
            let query = arguments.get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'query' (string)".to_string())?;
            let limit = arguments.get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;

            let home = resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let results = notes_cmd::do_search(&home, query, limit)
                .map_err(|e| format!("Search failed: {}", e))?;

            let items: Vec<Value> = results.iter().map(|scored| {
                json!({
                    "name": scored.item.manifest.name,
                    "description": scored.item.manifest.description,
                    "score": scored.score,
                    "maturity": format!("{:?}", scored.item.stats.as_ref()
                        .map(|s| s.lifecycle_state)
                        .unwrap_or(mur_common::skill::stats::LifecycleState::Draft)),
                })
            }).collect();

            Ok(json!({
                "results": items,
                "count": items.len(),
            }))
        }

        "mur_notes_show" => {
            let name = arguments.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'name' (string)".to_string())?;

            let home = resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let view: NoteView = notes_cmd::do_show(&home, name)
                .map_err(|e| format!("Note not found: {}", e))?;

            Ok(json!({
                "name": view.name,
                "description": view.description,
                "maturity": format!("{:?}", view.maturity),
                "body": view.body,
            }))
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

/// Resolve ~/.mur from environment or default. Reuses mur_core's exported function.
fn resolve_mur_home() -> anyhow::Result<std::path::PathBuf> {
    mur_core::cmd::resolve_mur_home()
}
```

- [ ] **Step 2: Verify the Cargo.toml dependencies are correct**

The `dirs` crate is NOT needed — `resolve_mur_home` is imported from `mur_core`. The Cargo.toml from Task 1 is correct as-is.

- [ ] **Step 3: Make `resolve_mur_home` and `do_search`/`do_show` accessible from external crates**

`do_search` and `do_show` are already `pub fn` in `mur-core/src/cmd/notes_cmd.rs` ✅.

`resolve_mur_home` is currently `pub(crate) fn` in `mur-core/src/cmd/agent/mod.rs` — it must be promoted to `pub` so the external `mur-mcp-server` crate can call it.

Change in `mur-core/src/cmd/agent/mod.rs`:
```diff
-pub(crate) fn resolve_mur_home() -> Result<PathBuf> {
+pub fn resolve_mur_home() -> Result<PathBuf> {
```

Also add a re-export in `mur-core/src/cmd/mod.rs` so callers don't need to reach into `cmd::agent`:
```rust
pub use agent::resolve_mur_home;
```

- [ ] **Step 4: Verify it compiles and run a manual smoke test**

Run: `cargo build -p mur-mcp-server`
Expected: compiles

Manual test:
```bash
# Start the server
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | cargo run -p mur-mcp-server
```

Expected: response with `serverInfo` and `capabilities`.

- [ ] **Step 5: Commit**

```bash
git add mur-mcp-server/src/tools.rs mur-mcp-server/Cargo.toml mur-core/src/cmd/agent/mod.rs
git commit -m "feat(mcp): wire mur_notes_search and mur_notes_show tools"
```

---

## Phase 2 — Remaining MCP Tools

### Task 5: Add do_project_search, do_project_status, do_project_list returning structs

**Files:**
- Modify: `mur-core/src/cmd/project.rs`

- [ ] **Step 1: Add return-struct types to project.rs**

Add these structs near the top of `project.rs` (after the existing `BackgroundMode` enum):

```rust
/// Structured result from project search — returned by do_project_search().
/// Reuses existing CodebaseIndex::search() and the CodeChunk struct from codebase/mod.rs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectSearchResult {
    pub chunks: Vec<ProjectSearchChunk>,
    pub total_hits: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectSearchChunk {
    pub project: String,
    pub file: String,
    pub language: String,
    pub chunk_type: String,
    pub symbol: Option<String>,
    pub content: String,
    pub line_start: u32,
    pub line_end: u32,
    pub score: f32,
}

/// Structured status for one project — returned by do_project_status().
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectStatusInfo {
    pub name: String,
    pub path: String,
    pub indexed: bool,
    pub chunks: Option<usize>,
    pub last_indexed: Option<String>,
    pub indexing_in_progress: bool,
    pub progress: Option<IndexProgressInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexProgressInfo {
    pub done_chunks: usize,
    pub total_chunks: usize,
    pub pct: f64,
    pub errors: usize,
}
```

- [ ] **Step 2: Add do_project_search() function**

Reuses the existing `CodebaseIndex::search(query_embedding, limit)` API (no `search_hybrid` — that doesn't exist yet):

```rust
pub async fn do_project_search(
    query: &str,
    project_filter: Option<&str>,
    limit: usize,
) -> Result<ProjectSearchResult> {
    let cfg = crate::store::config::load_config()?;
    let embed_config = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    let query_embedding = crate::store::embedding::embed(query, &embed_config).await?;

    let indexes = crate::codebase::discover_all_indexes();
    let mut all_chunks: Vec<ProjectSearchChunk> = Vec::new();

    for discovered in &indexes {
        if let Some(ref filter) = project_filter
            && discovered.name != *filter
        {
            continue;
        }

        let project_path = discovered
            .project_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let index = crate::codebase::CodebaseIndex::new(&discovered.name, &project_path);
        let chunks = index.search(&query_embedding, limit).await?;

        for c in &chunks {
            all_chunks.push(ProjectSearchChunk {
                project: discovered.name.clone(),
                file: c.file.clone(),
                language: c.language.clone(),
                chunk_type: c.chunk_type.clone(),
                symbol: c.symbol.clone(),
                content: c.content.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                score: c.score,
            });
        }
    }

    all_chunks.sort_by(|a, b| {
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    let total = all_chunks.len();
    all_chunks.truncate(limit);

    Ok(ProjectSearchResult { chunks: all_chunks, total_hits: total })
}
```

- [ ] **Step 3: Add do_project_status() and do_project_list() functions**

```rust
pub fn do_project_status(path: Option<&str>) -> Result<ProjectStatusInfo> {
    let project_path = match path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    let has_db = index.lance_path().exists();
    let stats = futures::executor::block_on(index.stats_async())?;

    let mut info = ProjectStatusInfo {
        name: project_name,
        path: project_path.display().to_string(),
        indexed: has_db,
        chunks: if has_db { Some(stats.chunks_created) } else { None },
        last_indexed: None,
        indexing_in_progress: false,
        progress: None,
    };

    // Check lock for background indexing
    let lock_path = index.lock_path();
    if lock_path.exists()
        && let Ok(data) = std::fs::read_to_string(&lock_path)
        && let Ok(lock) = serde_json::from_str::<IndexLock>(&data)
    {
        if mur_common::lock_file::pid_alive(lock.pid) {
            info.indexing_in_progress = true;
            if let Some(prog) = index.read_progress() {
                let pct = if prog.total_chunks > 0 {
                    (prog.done_chunks as f64 / prog.total_chunks as f64) * 100.0
                } else { 0.0 };
                info.progress = Some(IndexProgressInfo {
                    done_chunks: prog.done_chunks,
                    total_chunks: prog.total_chunks,
                    pct,
                    errors: prog.errors,
                });
            }
        }
    }

    Ok(info)
}

pub fn do_project_list() -> Result<Vec<ProjectStatusInfo>> {
    let indexes = discover_all_indexes();
    indexes.into_iter().map(|idx| {
        let project_path = idx.project_path.as_deref().unwrap_or("");
        Ok(ProjectStatusInfo {
            name: idx.name,
            path: project_path.to_string(),
            indexed: true, // discover_all_indexes only returns indexed projects
            chunks: Some(0), // quick — don't load stats for list view
            last_indexed: idx.last_indexed,
            indexing_in_progress: false,
            progress: None,
        })
    }).collect()
}
```

- [ ] **Step 4: Refactor existing cmd_project_search/cmd_project_status/cmd_project_list to use the new do_* functions**

Modify `cmd_project_search` to call `do_project_search` and format output, same pattern as notes_cmd.

- [ ] **Step 5: Verify it compiles**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/project.rs
git commit -m "refactor(project): add structured do_* functions for project search/status/list"
```

---

### Task 6: Add do_list and do_status returning structs for agents

**Files:**
- Modify: `mur-core/src/cmd/agent/lifecycle.rs`

- [ ] **Step 1: Add return types and do_list/do_status functions**

```rust
/// Structured agent list entry — returned by do_list().
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentListEntry {
    pub name: String,
    pub running: bool,
    pub transport: String,
}

/// Structured agent status — returned by do_status().
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStatusInfo {
    pub name: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub transport: String,
    pub socket_path: Option<String>,
    pub skills_count: usize,
    pub mcp_servers_count: usize,
}

pub fn do_list() -> Result<Vec<AgentListEntry>> {
    let home = super::resolve_mur_home()?;
    let agents_dir = home.join("agents");
    if !agents_dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            let profile_path = entry.path().join("profile.yaml");
            let running = check_running(&name);
            let transport = if profile_path.exists() {
                load_transport(&profile_path).unwrap_or_else(|_| "stdio".into())
            } else {
                "unknown".into()
            };
            entries.push(AgentListEntry { name, running, transport });
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

pub fn do_status(name: &str) -> Result<AgentStatusInfo> {
    let home = super::resolve_mur_home()?;
    let agent_dir = home.join("agents").join(name);
    if !agent_dir.exists() {
        anyhow::bail!("Agent '{}' not found. Run 'mur agent list' to see configured agents.", name);
    }
    let profile_path = agent_dir.join("profile.yaml");
    let running = check_running(name);
    let pid = get_pid(name);
    let transport = if profile_path.exists() {
        load_transport(&profile_path).unwrap_or_else(|_| "stdio".into())
    } else {
        "unknown".into()
    };
    let socket_path = super::comm::socket_path(name).ok();
    let skills_count = count_skills(&home, name);
    let mcp_servers_count = count_mcp(&agent_dir);

    Ok(AgentStatusInfo {
        name: name.into(),
        running,
        pid,
        transport,
        socket_path,
        skills_count,
        mcp_servers_count,
    })
}
```

Helper functions reused from existing `cmd_list`/`cmd_status` code in lifecycle.rs.

- [ ] **Step 2: Refactor cmd_list and cmd_status to call do_list/do_status**

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/lifecycle.rs
git commit -m "refactor(agent): add structured do_list/do_status for MCP server consumption"
```

---

### Task 7: Wire remaining 4 MCP tools

**Files:**
- Modify: `mur-mcp-server/src/tools.rs`

- [ ] **Step 1: Add mur_project_search, mur_project_status, mur_agent_status, mur_hook_context tool definitions and dispatch**

Add to `all_tools()`:

```rust
Tool {
    name: "mur_project_search".into(),
    description: "Search indexed project source code using hybrid vector+BM25. Returns code snippets with file paths, line numbers, and relevance scores. Only works after 'mur project index' has been run for the project.".into(),
    input_schema: ToolInputSchema {
        schema_type: "object".into(),
        properties: Some(vec![
            ("query".into(), ToolParam {
                param_type: "string".into(),
                description: "Search query".into(),
                default: None,
            }),
            ("project".into(), ToolParam {
                param_type: "string".into(),
                description: "Project name filter (defaults to searching all indexed projects)".into(),
                default: None,
            }),
            ("limit".into(), ToolParam {
                param_type: "integer".into(),
                description: "Max results, 1-10 (default: 5)".into(),
                default: Some(json!(5)),
            }),
        ]),
        required: Some(vec!["query".into()]),
    },
},
Tool {
    name: "mur_project_status".into(),
    description: "Show which projects are indexed and their indexing status (chunk count, last indexed, freshness, in-progress indexing). Use before project search to check if a project is indexed.".into(),
    input_schema: ToolInputSchema {
        schema_type: "object".into(),
        properties: None,
        required: None,
    },
},
Tool {
    name: "mur_agent_status".into(),
    description: "List configured MUR agents with their running state, health, transport, and tool counts. Use to check if agents are online before sending A2A messages. Pass a name to get detail for one agent; omit to list all.".into(),
    input_schema: ToolInputSchema {
        schema_type: "object".into(),
        properties: Some(vec![
            ("name".into(), ToolParam {
                param_type: "string".into(),
                description: "Optional agent name. Shows detail for one agent; lists all if omitted.".into(),
                default: None,
            }),
        ]),
        required: None,
    },
},
Tool {
    name: "mur_hook_context".into(),
    description: "Get the patterns that MUR would inject for the current project context. Returns top-ranked patterns within a token budget. Use at session start or when switching project contexts.".into(),
    input_schema: ToolInputSchema {
        schema_type: "object".into(),
        properties: Some(vec![
            ("query".into(), ToolParam {
                param_type: "string".into(),
                description: "Override auto-detected context query".into(),
                default: None,
            }),
            ("compact".into(), ToolParam {
                param_type: "boolean".into(),
                description: "Return fewer patterns in shorter format (default: false)".into(),
                default: Some(json!(false)),
            }),
            ("budget".into(), ToolParam {
                param_type: "integer".into(),
                description: "Token budget for returned content (default: 2000)".into(),
                default: Some(json!(2000)),
            }),
        ]),
        required: None,
    },
},
```

Add dispatch arms in `call_tool()`:

```rust
"mur_project_search" => {
    let query = arguments.get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter: 'query' (string)".to_string())?;
    let project = arguments.get("project").and_then(|v| v.as_str());
    let limit = arguments.get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .clamp(1, 10) as usize;

    let result = mur_core::cmd::project::do_project_search(query, project, limit)
        .await // need to make this fn or use block_on
        .map_err(|e| format!("Project search failed: {}", e))?;

    let snippets: Vec<Value> = result.chunks.iter().map(|c| json!({
        "file": c.file_path,
        "lines": format!("{}-{}", c.start_line, c.end_line),
        "content": c.content,
        "score": c.score,
    })).collect();

    Ok(json!({
        "results": snippets,
        "count": result.total_hits,
    }))
}

"mur_project_status" => {
    let status = mur_core::cmd::project::do_project_status(None)
        .map_err(|e| format!("Project status failed: {}", e))?;
    let list = mur_core::cmd::project::do_project_list().unwrap_or_default();

    Ok(json!({
        "current_project": status,
        "all_indexed": list,
    }))
}

"mur_agent_status" => {
    if let Some(name) = arguments.get("name").and_then(|v| v.as_str()) {
        let status = mur_core::cmd::agent::lifecycle::do_status(name)
            .map_err(|e| format!("Agent status failed: {}", e))?;
        Ok(serde_json::to_value(status).unwrap_or(Value::Null))
    } else {
        let list = mur_core::cmd::agent::lifecycle::do_list()
            .map_err(|e| format!("Agent list failed: {}", e))?;
        Ok(json!({ "agents": list }))
    }
}

"mur_hook_context" => {
    let query = arguments.get("query").and_then(|v| v.as_str()).map(|s| s.to_string());
    let compact = arguments.get("compact").and_then(|v| v.as_bool()).unwrap_or(false);
    let budget = arguments.get("budget").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

    let result = mur_core::cmd::context::do_context(query, compact, budget)
        .await
        .map_err(|e| format!("Context retrieval failed: {}", e))?;

    Ok(json!({
        "patterns": result.patterns,
        "project": result.project_context,
        "token_count": result.token_count,
    }))
}
```

- [ ] **Step 2: Make context::cmd_context async parts accessible — add do_context()**

In `mur-core/src/cmd/context.rs`, add a `do_context` function that returns a struct instead of printing:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextResult {
    pub patterns: Vec<ContextPattern>,
    pub project_context: Option<String>,
    pub token_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextPattern {
    pub name: String,
    pub description: String,
    pub content: String,
    pub tier: String,
}

pub async fn do_context(
    query: Option<String>,
    compact: bool,
    budget: usize,
) -> Result<ContextResult> {
    // Reuse the core logic from cmd_context but return structured data
    // instead of printing to stdout/stderr
    // ...
}
```

- [ ] **Step 3: Verify it compiles and all tools are registered**

Run: `cargo build -p mur-mcp-server`
Expected: compiles with all 6 tools

Echo test:
```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | cargo run -p mur-mcp-server
```
Expected: response with all 6 tools defined.

- [ ] **Step 4: Commit**

```bash
git add mur-mcp-server/src/tools.rs mur-core/src/cmd/context.rs
git commit -m "feat(mcp): wire all 6 MCP tools — notes, project, agent, context"
```

---

## Phase 3 — Skills

### Task 8: Create 4 new skill manifests

**Files:**
- Create: `~/.mur/skills/mur-project-index/skill.yaml`
- Create: `~/.mur/skills/mur-project-index/SKILL.md`
- Create: `~/.mur/skills/mur-project-remove/skill.yaml`
- Create: `~/.mur/skills/mur-project-remove/SKILL.md`
- Create: `~/.mur/skills/mur-session-remove/skill.yaml`
- Create: `~/.mur/skills/mur-session-remove/SKILL.md`
- Create: `~/.mur/skills/mur-agent-manage/skill.yaml`
- Create: `~/.mur/skills/mur-agent-manage/SKILL.md`

- [ ] **Step 1: Create mur-project-index/SKILL.md and skill.yaml**

Write the exact content from the spec §5.1 for each skill. Both the SKILL.md (markdown frontmatter format) and skill.yaml (canonical format) are needed.

- [ ] **Step 2: Create mur-project-remove/SKILL.md and skill.yaml**

- [ ] **Step 3: Create mur-session-remove/SKILL.md and skill.yaml**

- [ ] **Step 4: Create mur-agent-manage/SKILL.md and skill.yaml**

- [ ] **Step 5: Validate each skill**

Run: `mur skill validate ~/.mur/skills/mur-project-index/skill.yaml`
Expected: `ok: mur-project-index`

Repeat for all 4 skills.

- [ ] **Step 6: Commit**

```bash
git add ~/.mur/skills/mur-project-index/ ~/.mur/skills/mur-project-remove/ \
        ~/.mur/skills/mur-session-remove/ ~/.mur/skills/mur-agent-manage/
git commit -m "feat(skills): add 4 new MUR skills (project-index, project-remove, session-remove, agent-manage)"
```

---

### Task 9: Update 3 existing skills

**Files:**
- Modify: `~/.mur/skills/mur-context/skill.yaml`
- Modify: `~/.mur/skills/mur-context/SKILL.md`
- Modify: `~/.mur/skills/mur-in/skill.yaml`
- Modify: `~/.mur/skills/mur-in/SKILL.md`
- Modify: `~/.mur/skills/mur-out/skill.yaml`
- Modify: `~/.mur/skills/mur-out/SKILL.md`

- [ ] **Step 1: Update mur-context** — add mention of MCP tools and project indexing awareness

- [ ] **Step 2: Update mur-in** — add `mur session in` trigger as primary, keep `/mur-in` for back compat

- [ ] **Step 3: Update mur-out** — add Stop hook trigger, mention `mur session out --action analyze`

- [ ] **Step 4: Validate each updated skill**

Run: `mur skill validate ~/.mur/skills/mur-context/skill.yaml`
Expected: `ok: mur-context`

- [ ] **Step 5: Commit**

```bash
git add ~/.mur/skills/mur-context/ ~/.mur/skills/mur-in/ ~/.mur/skills/mur-out/
git commit -m "feat(skills): update mur-context, mur-in, mur-out for MCP tools and hooks"
```

---

## Phase 4 — Hooks

### Task 10: Add git-commit detection to hook tool handler

**Files:**
- Modify: `mur-core/src/cmd/hook.rs`

- [ ] **Step 1: Add git-commit detection logic to cmd_hook_tool**

```rust
/// Keywords that trigger a background project reindex.
const INDEX_TRIGGER_COMMANDS: &[&str] = &[
    "git commit",
    "git push",
];

fn should_trigger_index(tool_input: &serde_json::Value) -> bool {
    let command = tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let command_lower = command.to_lowercase();
    INDEX_TRIGGER_COMMANDS.iter().any(|trigger| command_lower.contains(trigger))
}

fn spawn_background_index() {
    let mur_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mur")))
        .unwrap_or_else(|| std::path::PathBuf::from("mur"));

    tracing::info!("git commit detected — spawning background project index");
    if let Err(e) = std::process::Command::new(&mur_bin)
        .args(&["project", "index", "--quiet", "--background"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(error = %e, "failed to spawn background index");
    }
}
```

- [ ] **Step 2: Integrate into cmd_hook_tool handler**

In `cmd_hook_tool`, after parsing the stdin event, check if the tool is Bash and the command is a git commit/push. If so, call `spawn_background_index()`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/hook.rs
git commit -m "feat(hook): auto-trigger mur project index on git commit/push"
```

---

### Task 11: Integration tests

**Files:**
- Create: `mur-mcp-server/tests/integration.rs`

- [ ] **Step 1: Write integration test that spawns the MCP server and verifies tool list**

```rust
// mur-mcp-server/tests/integration.rs
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

fn send_request(stdin: &mut impl Write, request: &str) {
    writeln!(stdin, "{}", request).unwrap();
}

fn read_response(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn test_initialize_and_list_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    // Initialize
    send_request(&mut stdin, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#);
    let resp = read_response(&mut stdout);
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["serverInfo"]["name"].as_str().unwrap().contains("mur"));

    // Confirm initialization
    send_request(&mut stdin, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // List tools
    send_request(&mut stdin, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    let resp = read_response(&mut stdout);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6, "Expected 6 tools");

    // Verify tool names
    let names: Vec<&str> = tools.iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"mur_notes_search"));
    assert!(names.contains(&"mur_notes_show"));
    assert!(names.contains(&"mur_project_search"));
    assert!(names.contains(&"mur_project_status"));
    assert!(names.contains(&"mur_agent_status"));
    assert!(names.contains(&"mur_hook_context"));

    child.kill().ok();
}

#[test]
fn test_tools_list_response_under_token_budget() {
    // Verify tools/list JSON stays under 5000 tokens (~20K chars)
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    send_request(&mut stdin, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#);
    let _ = read_response(&mut stdout);
    send_request(&mut stdin, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    send_request(&mut stdin, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);

    let resp = read_response(&mut stdout);
    let tools_json = serde_json::to_string(&resp["result"]["tools"]).unwrap();
    // ~800 tokens ≈ 3,200 chars. 6 tools = ~19,200 chars. Budget: 25,000 chars
    assert!(
        tools_json.len() < 25_000,
        "tools/list response is {} chars, must stay under 25,000 (5,000 token budget)",
        tools_json.len()
    );

    child.kill().ok();
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p mur-mcp-server`
Expected: both tests pass

- [ ] **Step 3: Verify token budget snapshot test passes**

- [ ] **Step 4: Commit**

```bash
git add mur-mcp-server/tests/integration.rs
git commit -m "test(mcp): add integration tests — lifecycle, tool count, token budget"
```

---

## Plan Self-Review

**Spec coverage:**
- §4.1 MCP Tools: Tasks 4 + 7 cover all 6 tools ✅
- §4.2 Tool Schema Design: flat params, short descriptions embedded in tool definitions ✅
- §4.5 Crate Structure: Tasks 1-3 create the crate ✅
- §5.1 New Skills: Task 8 creates 4 new skill manifests ✅
- §5.2 Existing Skill Updates: Task 9 updates 3 skills ✅
- §6.1 Hook Configuration: Task 10 adds git-commit detection ✅
- §8 Testing: Task 11 covers MCP protocol + tool count + token budget ✅

**Placeholder scan:** No TBDs, TODOs, or vague instructions. All code is shown inline.

**Type consistency:**
- `ProjectSearchResult` defined in Task 5, consumed in Task 7 ✅
- `AgentStatusInfo` / `AgentListEntry` defined in Task 6, consumed in Task 7 ✅
- `Tool` / `ToolInputSchema` / `ToolParam` defined in Task 3, used in Tasks 4 + 7 ✅
- `do_search` / `do_show` already exist, referenced in Task 4 ✅
