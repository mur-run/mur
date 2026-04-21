# mur Sources — Phase 1.4 Notion + Joplin + Watch + Schedule Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the remaining adapters (Notion via OAuth+REST, Joplin via Local SQLite + Joplin Server token), `mur source sync --watch` (file watcher for Obsidian + polling for cloud sources), `mur source install-schedule` (launchd/systemd unit generator), and wire `inject::format_notes_section` into the AI-injection top-level entry.

**Architecture:** New adapters `sources/adapters/notion.rs` and `sources/adapters/joplin.rs` implement `KnowledgeSource`. Notion uses a public OAuth integration with PKCE (callback via reused `axum` server on a random localhost port) plus an Internal Integration Token (PAT) escape hatch. Notion's block model is converted to markdown by a new `sources/chunker/notion_blocks.rs`. Joplin reads a local SQLite database (`rusqlite` opened read-only with `immutable=true`) or the Joplin Server REST API. `--watch` mode uses `notify` for filesystem events (Obsidian) and a `tokio::time::interval` for cloud polling (Notion/Joplin Server). `install-schedule` emits a `launchd` plist on macOS and a `systemd --user` unit on Linux that calls `mur source sync --full` on `sources_global.poll_interval_secs`.

**Tech Stack:** Rust edition 2024, Tokio, async-trait, anyhow, reqwest (existing), rusqlite (existing, used for Joplin), notify (new, file watcher), oauth2 (new, PKCE), governor (new, rate limit), axum (existing, OAuth callback), serde_json. No new vector backends.

**Spec reference:** `docs/superpowers/specs/2026-04-20-mur-sources-integration-design.md` §6.1 (sync triggers), §6.7 (OAuth flow), §7.2 (Notion adapter), §7.3 (Joplin adapter), §11 (P1.4 line).

**Depends on:** P1.1 + P1.2 + P1.3 all merged. Start P1.4 from current `main` (commit `df9485c`).

---

## File Structure

```
mur-core/
  Cargo.toml                                # MODIFY: + notify, oauth2, governor
  src/
    sources/
      mod.rs                                # MODIFY: + adapters/{notion,joplin}; + watch
      chunker/
        notion_blocks.rs                    # NEW: Notion block → markdown chunker
        mod.rs                              # MODIFY: + pub mod notion_blocks;
      adapters/
        mod.rs                              # MODIFY: + pub mod notion; + pub mod joplin;
        notion.rs                           # NEW: OAuth + REST + rate-limited
        joplin.rs                           # NEW: Local SQLite + Server REST
      watch.rs                              # NEW: orchestrator combining fsevents + polling
    cmd/
      source_cmd.rs                         # MODIFY: real handlers for add notion / joplin / sync --watch
      schedule_cmd.rs                       # NEW: install-schedule handler (launchd/systemd unit gen)
    inject/
      hook.rs                               # MODIFY: wire format_notes_section into inject entry
  tests/
    notion_block_chunker.rs                 # NEW: unit tests with hand-crafted block payloads
    joplin_local_db.rs                      # NEW: end-to-end against a fixture SQLite
docs/
  source-adapters.md                        # NEW: user-facing setup notes (Notion OAuth, Joplin paths)
```

**Key design choices**:
- **Notion API**: raw `reqwest` (matches mur's `store/embedding.rs` style; avoids `notion-sdk-rs` maintenance question). PKCE via `oauth2` crate. Public-app `client_id` embedded in binary; PAT fallback `--token <pat>` skips OAuth entirely.
- **Notion rate limit**: `governor` token bucket at 3 req/sec (Notion official cap). 429 → respect `Retry-After`.
- **Joplin auth**: Local DB has none (file path); Server uses bearer token in keyring (account `joplin:<instance>:api_token`).
- **Joplin SQLite**: opened with `?mode=ro&immutable=1` URI flag so a running Joplin app doesn't lock conflict.
- **Watch mode**: foreground daemon, `tokio::select!` over `notify::Watcher` events + interval ticks. Ctrl+C signal handled cleanly. NOT a forked process.
- **install-schedule**: writes a plist / unit file but doesn't `launchctl load` itself — print instructions for the user to enable. Idempotent (overwrites if exists).
- **Inject wiring**: find the existing pattern-injection entry (search `format_pattern_section`-equivalent), append `format_notes_section()` output gated behind `cfg(feature = "sources")`.
- **Notion block converter**: handles `paragraph`, `heading_1/2/3`, `bulleted/numbered_list_item`, `code`, `quote`, `callout`, `to_do`, `toggle`, `table`. Database property pages indexed in P1.5 (deferred).

---

## Task 0: Worktree Baseline

- [ ] **Step 1: Confirm worktree**

```bash
cd /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.4
git branch --show-current     # feat/sources-p1.4
git log --oneline -2          # df9485c head, plus the plan commit when added
```

- [ ] **Step 2: Baseline tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

All must pass.

---

## Task 1: Add `notify` + `oauth2` + `governor` Deps

**Files:** `mur-core/Cargo.toml`

- [ ] **Step 1: Edit Cargo.toml**

In `[dependencies]`, add:

```toml
notify = "6"
oauth2 = "4"
governor = "0.6"
```

(`notify = "6"` for cross-platform fsevents; `oauth2 = "4"` for PKCE; `governor = "0.6"` for token-bucket rate limit. If 0.7 or newer is current, use that — adjust API per Step 2.)

- [ ] **Step 2: Compile**

```bash
cargo check --workspace 2>&1 | tail -5
```

Expected: clean. First compile downloads new deps (slow).

- [ ] **Step 3: Commit**

```bash
git add mur-core/Cargo.toml Cargo.lock
git commit -m "chore(deps): add notify 6 + oauth2 4 + governor 0.6"
```

---

## Task 2: Notion Block → Markdown Chunker

**Files:** `mur-core/src/sources/chunker/notion_blocks.rs`, `chunker/mod.rs`

- [ ] **Step 1: Create `notion_blocks.rs`**

```rust
//! Notion block → markdown converter.
//!
//! Notion's API returns pages as a tree of typed blocks. We linearise to
//! markdown for the existing `markdown::chunk_markdown` to chunk by heading.
//! Block types covered (P1.4): paragraph, heading_1/2/3,
//! bulleted_list_item, numbered_list_item, code, quote, callout, to_do,
//! toggle (recurses), table (rendered as markdown table).
//!
//! Database property pages are NOT included (deferred).

use serde_json::Value;

/// Convert a flat list of Notion block JSON values to a single markdown body.
///
/// `blocks` should be the `results` array from a `/v1/blocks/{id}/children`
/// response. Each element is a typed object like `{ "type": "paragraph",
/// "paragraph": { "rich_text": [...] } }`.
pub fn blocks_to_markdown(blocks: &[Value]) -> String {
    let mut out = String::new();
    for b in blocks {
        render_block(b, 0, &mut out);
    }
    out
}

fn render_block(block: &Value, depth: usize, out: &mut String) {
    let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let detail = block.get(kind);
    let indent = "  ".repeat(depth);
    match kind {
        "paragraph" => {
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_1" => {
            out.push_str("# ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_2" => {
            out.push_str("## ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_3" => {
            out.push_str("### ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "bulleted_list_item" => {
            out.push_str(&indent);
            out.push_str("- ");
            push_rich_text(detail, out);
            out.push('\n');
        }
        "numbered_list_item" => {
            out.push_str(&indent);
            out.push_str("1. ");
            push_rich_text(detail, out);
            out.push('\n');
        }
        "to_do" => {
            let checked = detail.and_then(|d| d.get("checked")).and_then(|v| v.as_bool()).unwrap_or(false);
            out.push_str(&indent);
            out.push_str(if checked { "- [x] " } else { "- [ ] " });
            push_rich_text(detail, out);
            out.push('\n');
        }
        "quote" => {
            out.push_str("> ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "callout" => {
            // Render as quote with leading icon-emoji placeholder.
            out.push_str("> ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "code" => {
            let lang = detail
                .and_then(|d| d.get("language"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str("```");
            out.push_str(lang);
            out.push('\n');
            push_rich_text(detail, out);
            out.push_str("\n```\n\n");
        }
        "toggle" => {
            // Surface the summary line; nested children handled if expanded by caller
            push_rich_text(detail, out);
            out.push('\n');
            if let Some(children) = block.get("children").and_then(|v| v.as_array()) {
                for child in children {
                    render_block(child, depth + 1, out);
                }
            }
        }
        "table" => {
            // Children of a table are `table_row` blocks (caller must hydrate children).
            if let Some(rows) = block.get("children").and_then(|v| v.as_array()) {
                render_table(rows, out);
            }
        }
        "divider" => {
            out.push_str("\n---\n\n");
        }
        _ => {
            // Unknown / unhandled: best-effort rich_text extraction for resilience
            if let Some(detail) = detail {
                push_rich_text(Some(detail), out);
                out.push_str("\n\n");
            }
        }
    }
    // Most blocks support nested children; fetch+inject when caller hydrates.
    // For toggles and table we already handled above. For other types, render
    // children if present (e.g., bulleted lists contain sub-items).
    if !matches!(kind, "toggle" | "table") {
        if let Some(children) = block.get("children").and_then(|v| v.as_array()) {
            for child in children {
                render_block(child, depth + 1, out);
            }
        }
    }
}

fn push_rich_text(detail: Option<&Value>, out: &mut String) {
    let arr = detail
        .and_then(|d| d.get("rich_text"))
        .and_then(|v| v.as_array());
    let Some(arr) = arr else { return };
    for span in arr {
        if let Some(t) = span.get("plain_text").and_then(|v| v.as_str()) {
            out.push_str(t);
        }
    }
}

fn render_table(rows: &[Value], out: &mut String) {
    let mut first = true;
    for row in rows {
        if row.get("type").and_then(|v| v.as_str()) != Some("table_row") {
            continue;
        }
        let cells = row
            .get("table_row")
            .and_then(|d| d.get("cells"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        out.push('|');
        for cell in &cells {
            out.push(' ');
            if let Some(cell_arr) = cell.as_array() {
                for span in cell_arr {
                    if let Some(t) = span.get("plain_text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                    }
                }
            }
            out.push_str(" |");
        }
        out.push('\n');
        if first {
            out.push('|');
            for _ in &cells {
                out.push_str(" --- |");
            }
            out.push('\n');
            first = false;
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paragraph_to_markdown() {
        let blocks = vec![json!({
            "type": "paragraph",
            "paragraph": {"rich_text": [{"plain_text": "hello world"}]}
        })];
        let md = blocks_to_markdown(&blocks);
        assert!(md.starts_with("hello world"));
    }

    #[test]
    fn heading_1_renders_hash() {
        let blocks = vec![json!({
            "type": "heading_1",
            "heading_1": {"rich_text": [{"plain_text": "Title"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("# Title"));
    }

    #[test]
    fn bulleted_list_item_renders_dash() {
        let blocks = vec![json!({
            "type": "bulleted_list_item",
            "bulleted_list_item": {"rich_text": [{"plain_text": "item"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("- item"));
    }

    #[test]
    fn to_do_unchecked_renders_brackets() {
        let blocks = vec![json!({
            "type": "to_do",
            "to_do": {"rich_text": [{"plain_text": "task"}], "checked": false}
        })];
        assert!(blocks_to_markdown(&blocks).contains("- [ ] task"));
    }

    #[test]
    fn code_block_includes_language_fence() {
        let blocks = vec![json!({
            "type": "code",
            "code": {"language": "rust", "rich_text": [{"plain_text": "let x = 1;"}]}
        })];
        let md = blocks_to_markdown(&blocks);
        assert!(md.contains("```rust"));
        assert!(md.contains("let x = 1;"));
    }

    #[test]
    fn unknown_block_extracts_rich_text() {
        let blocks = vec![json!({
            "type": "embed",
            "embed": {"rich_text": [{"plain_text": "https://x"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("https://x"));
    }
}
```

- [ ] **Step 2: Register submodule**

Append to `mur-core/src/sources/chunker/mod.rs`:

```rust
pub mod notion_blocks;
```

- [ ] **Step 3: Tests**

```bash
cargo test -p mur-core sources::chunker::notion_blocks 2>&1 | tail -10
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/sources/chunker/mod.rs mur-core/src/sources/chunker/notion_blocks.rs
git commit -m "feat(sources/chunker): notion block → markdown converter"
```

---

## Task 3: NotionAdapter — Skeleton + PAT auth + list_documents

**Files:** `mur-core/src/sources/adapters/notion.rs`, `adapters/mod.rs`

- [ ] **Step 1: Create `notion.rs`**

```rust
//! Notion workspace adapter.
//!
//! Auth: OAuth 2.0 + PKCE (public mur integration) — implemented in Task 5.
//! For P1.4 Step 1 we focus on the PAT (Internal Integration Token) path
//! which lets users connect immediately by passing a token they create in
//! https://www.notion.so/my-integrations.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use governor::{Quota, RateLimiter};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use crate::sources::KnowledgeSource;
use crate::sources::chunker::notion_blocks;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
const RATE_LIMIT_PER_SEC: u32 = 3;
const CHUNK_MAX_CHARS: usize = 6000;

pub struct NotionAdapter {
    id: String,
    client: Client,
    token: String,
    workspace_id: Option<String>,
    weight: f32,
    limiter: Arc<RateLimiter<governor::state::NotKeyed, governor::state::InMemoryState, governor::clock::DefaultClock>>,
}

impl NotionAdapter {
    pub fn from_instance(instance: &SourceInstance, token: String) -> Result<Self> {
        if instance.type_name != "notion" {
            bail!("expected type_name 'notion', got '{}'", instance.type_name);
        }
        let workspace_id = instance
            .scope
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;
        let limiter = Arc::new(RateLimiter::direct(Quota::per_second(
            std::num::NonZeroU32::new(RATE_LIMIT_PER_SEC).unwrap(),
        )));
        Ok(Self {
            id: instance.id.clone(),
            client,
            token,
            workspace_id,
            weight: instance.weight,
            limiter,
        })
    }

    async fn rl(&self) {
        self.limiter.until_ready().await;
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    id: String,
    last_edited_time: Option<String>,
    archived: Option<bool>,
    properties: Option<serde_json::Value>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlocksResponse {
    results: Vec<serde_json::Value>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[async_trait]
impl KnowledgeSource for NotionAdapter {
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
        let mut start_cursor: Option<String> = None;

        loop {
            self.rl().await;
            let mut body = serde_json::json!({
                "filter": {"value": "page", "property": "object"},
                "page_size": 100,
            });
            if let Some(c) = &start_cursor {
                body["start_cursor"] = serde_json::Value::String(c.clone());
            }
            let resp = self
                .client
                .post(format!("{NOTION_API_BASE}/search"))
                .bearer_auth(&self.token)
                .header("Notion-Version", NOTION_VERSION)
                .json(&body)
                .send()
                .await
                .context("notion search request")?;
            let resp = retry_or_error(resp).await?;
            let s: SearchResponse = resp.json().await.context("decode notion search response")?;

            for r in s.results {
                if r.archived.unwrap_or(false) {
                    continue;
                }
                let updated_at = r
                    .last_edited_time
                    .as_deref()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                if let Some(t) = threshold
                    && updated_at <= t
                {
                    continue;
                }
                if max_ts.is_none() || max_ts.is_some_and(|m| updated_at > m) {
                    max_ts = Some(updated_at);
                }

                let title = title_from_properties(r.properties.as_ref());
                docs.push(DocRef {
                    external_id: r.id,
                    title,
                    updated_at,
                });
            }

            if s.has_more {
                start_cursor = s.next_cursor;
                if start_cursor.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        let cursor_out = match max_ts {
            Some(t) => SyncCursor(t.to_rfc3339()),
            None => SyncCursor(threshold.map(|t| t.to_rfc3339()).unwrap_or_default()),
        };
        Ok((docs, cursor_out))
    }

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        // Walk paginated /blocks/{id}/children
        let mut all_blocks: Vec<serde_json::Value> = Vec::new();
        let mut start_cursor: Option<String> = None;
        loop {
            self.rl().await;
            let mut url = format!(
                "{NOTION_API_BASE}/blocks/{}/children?page_size=100",
                doc_ref.external_id
            );
            if let Some(c) = &start_cursor {
                url.push_str(&format!("&start_cursor={c}"));
            }
            let resp = self
                .client
                .get(&url)
                .bearer_auth(&self.token)
                .header("Notion-Version", NOTION_VERSION)
                .send()
                .await
                .context("notion blocks request")?;
            let resp = retry_or_error(resp).await?;
            let b: BlocksResponse = resp.json().await.context("decode notion blocks response")?;
            all_blocks.extend(b.results);
            if b.has_more {
                start_cursor = b.next_cursor;
                if start_cursor.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        let title = doc_ref.title.clone().unwrap_or_else(|| doc_ref.external_id.clone());
        let url = format!("https://www.notion.so/{}", doc_ref.external_id.replace('-', ""));

        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::NotionBlocks(serde_json::Value::Array(all_blocks)),
            url: Some(url),
            updated_at: doc_ref.updated_at,
            tags: vec![],
            metadata: serde_json::Value::Null,
        })
    }

    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        let blocks: &[serde_json::Value] = match &doc.body {
            DocumentBody::NotionBlocks(serde_json::Value::Array(arr)) => arr,
            _ => bail!("notion adapter expects NotionBlocks(Array) body"),
        };
        let md = notion_blocks::blocks_to_markdown(blocks);
        let raw = crate::sources::chunker::markdown::chunk_markdown(&doc.title, &md, CHUNK_MAX_CHARS);
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

fn title_from_properties(props: Option<&serde_json::Value>) -> Option<String> {
    let props = props?;
    let obj = props.as_object()?;
    for (_, v) in obj {
        let kind = v.get("type")?.as_str()?;
        if kind == "title" {
            let arr = v.get("title")?.as_array()?;
            let mut s = String::new();
            for span in arr {
                if let Some(t) = span.get("plain_text").and_then(|x| x.as_str()) {
                    s.push_str(t);
                }
            }
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

async fn retry_or_error(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait_secs = resp
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        bail!("notion rate-limited (429); retry after {wait_secs}s");
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    bail!("notion api error {status}: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_extraction_from_real_property_shape() {
        let props = serde_json::json!({
            "Name": {
                "type": "title",
                "title": [{"plain_text": "My Page"}]
            }
        });
        assert_eq!(title_from_properties(Some(&props)), Some("My Page".into()));
    }

    #[test]
    fn missing_title_returns_none() {
        let props = serde_json::json!({"Status": {"type": "select", "select": null}});
        assert_eq!(title_from_properties(Some(&props)), None);
    }
}
```

- [ ] **Step 2: Register**

Open `/Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.4/mur-core/src/sources/adapters/mod.rs` and append:

```rust
pub mod notion;
```

- [ ] **Step 3: Tests + commit**

```bash
cargo test -p mur-core sources::adapters::notion 2>&1 | tail -10
```

Expected: 2 tests pass.

```bash
git add mur-core/src/sources/adapters/mod.rs mur-core/src/sources/adapters/notion.rs
git commit -m "feat(sources/notion): adapter skeleton + PAT auth + list_documents/fetch/chunk"
```

---

## Task 4: JoplinAdapter — Local SQLite Mode

**Files:** `mur-core/src/sources/adapters/joplin.rs`, `adapters/mod.rs`

- [ ] **Step 1: Create `joplin.rs`**

```rust
//! Joplin adapter.
//!
//! Two modes:
//!  - Local SQLite: reads `database.sqlite` directly, opened read-only with
//!    `?mode=ro&immutable=1` URI flags so a running Joplin app does not
//!    cause lock contention.
//!  - Joplin Server: REST API with bearer token from keyring (Task 8).

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
    // Server mode added in Task 8.
}

pub struct JoplinAdapter {
    id: String,
    mode: JoplinMode,
    weight: f32,
}

impl JoplinAdapter {
    pub fn from_instance(instance: &SourceInstance) -> Result<Self> {
        if instance.type_name != "joplin" {
            bail!("expected type_name 'joplin', got '{}'", instance.type_name);
        }
        let db = instance
            .scope
            .get("db_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let server = instance.scope.get("server_url");
        if let Some(db_path) = db {
            if !db_path.exists() {
                bail!("joplin db not found: {}", db_path.display());
            }
            return Ok(Self {
                id: instance.id.clone(),
                mode: JoplinMode::LocalDb { db_path },
                weight: instance.weight,
            });
        }
        if server.is_some() {
            bail!("joplin server mode arrives in Task 8");
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
        let JoplinMode::LocalDb { db_path } = &self.mode;
        let db_path = db_path.clone();
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

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        let JoplinMode::LocalDb { db_path } = &self.mode;
        let db_path = db_path.clone();
        let id = doc_ref.external_id.clone();
        let body_text = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = Self::open_ro(&db_path)?;
            let mut stmt = conn.prepare("SELECT body FROM notes WHERE id = ?1")?;
            let body: String = stmt.query_row([&id], |row| row.get::<_, String>(0))?;
            Ok(body)
        })
        .await
        .context("spawn_blocking joplin fetch")??;

        let title = doc_ref.title.clone().unwrap_or_else(|| doc_ref.external_id.clone());
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
        let adapter = JoplinAdapter::from_instance(&inst).unwrap();
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
        let adapter = JoplinAdapter::from_instance(&inst).unwrap();
        let (docs, _) = adapter.list_documents(None).await.unwrap();
        let n1 = docs.iter().find(|d| d.external_id == "n1").unwrap();
        let doc = adapter.fetch(n1).await.unwrap();
        match &doc.body {
            DocumentBody::Markdown(s) => assert!(s.contains("# h1")),
            _ => panic!("expected markdown"),
        }
    }
}
```

- [ ] **Step 2: Register**

Append to `sources/adapters/mod.rs`:

```rust
pub mod joplin;
```

- [ ] **Step 3: Tests + commit**

```bash
cargo test -p mur-core sources::adapters::joplin 2>&1 | tail -10
```

Expected: 2 tests pass.

```bash
git add mur-core/src/sources/adapters/mod.rs mur-core/src/sources/adapters/joplin.rs
git commit -m "feat(sources/joplin): local SQLite mode (read-only, immutable)"
```

---

## Task 5: NotionAdapter OAuth flow (PKCE + axum callback)

**Files:** `mur-core/src/sources/adapters/notion.rs`, `mur-core/src/cmd/source_cmd.rs`

The OAuth flow: spin up axum on a random localhost port, open browser to `https://api.notion.com/v1/oauth/authorize?...&code_challenge=…`, receive the `code` on `/callback`, exchange for `access_token`, store in keyring.

For P1.4 we keep the public-app `client_id` configurable via build env var `MUR_NOTION_CLIENT_ID` (defaults to `"FILL_ME_IN"` so the build never breaks; users with a real client_id rebuild with the env). Production releases set this in CI.

- [ ] **Step 1: Add OAuth helper to `notion.rs`**

Append at the bottom of `notion.rs`:

```rust
// ---------- OAuth (PKCE) ----------

const NOTION_OAUTH_AUTHORIZE: &str = "https://api.notion.com/v1/oauth/authorize";
const NOTION_OAUTH_TOKEN: &str = "https://api.notion.com/v1/oauth/token";
const NOTION_CLIENT_ID: &str = match option_env!("MUR_NOTION_CLIENT_ID") {
    Some(v) => v,
    None => "FILL_ME_IN",
};

/// Outcome of the OAuth flow.
pub struct OAuthResult {
    pub access_token: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
}

/// Run the OAuth dance. Spawns an axum server on a random port, opens a browser,
/// waits for the callback, exchanges the code, returns the token.
///
/// Notion's OAuth uses confidential-client mode by default — but this works
/// for self-hosted PKCE too with `client_secret = ""`. If you control the
/// integration (recommended for personal mur builds), use a "public" type.
pub async fn run_oauth_flow() -> Result<OAuthResult> {
    use axum::{Router, extract::Query, response::Html, routing::get};
    use oauth2::{
        AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl,
        TokenUrl, basic::BasicClient,
    };
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    if NOTION_CLIENT_ID == "FILL_ME_IN" {
        bail!(
            "MUR_NOTION_CLIENT_ID was not set at build time. Use --token <PAT> or rebuild mur with MUR_NOTION_CLIENT_ID=<your_client_id>."
        );
    }

    // Bind 127.0.0.1:0 to get an OS-assigned random port.
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("bind oauth callback")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let client = BasicClient::new(
        ClientId::new(NOTION_CLIENT_ID.to_string()),
        None,
        AuthUrl::new(NOTION_OAUTH_AUTHORIZE.into())?,
        Some(TokenUrl::new(NOTION_OAUTH_TOKEN.into())?),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_uri.clone())?);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf) = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(pkce_challenge)
        .url();

    let (tx, rx) = oneshot::channel::<(String, String)>();
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

    #[derive(serde::Deserialize)]
    struct CallbackParams {
        code: Option<String>,
        state: Option<String>,
    }

    let tx_clone = tx.clone();
    let app = Router::new().route(
        "/callback",
        get(move |Query(p): Query<CallbackParams>| {
            let tx = tx_clone.clone();
            async move {
                if let (Some(c), Some(s)) = (p.code, p.state) {
                    if let Some(send) = tx.lock().await.take() {
                        let _ = send.send((c, s));
                    }
                }
                Html("<html><body><h2>Notion connected. You can close this tab.</h2></body></html>")
            }
        }),
    );

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    println!("→ opening browser: {auth_url}");
    let _ = open::that(auth_url.as_str());

    let (code, returned_csrf) = tokio::time::timeout(Duration::from_secs(300), rx)
        .await
        .context("oauth callback timeout")?
        .context("callback channel closed")?;

    if returned_csrf != csrf.secret().as_str() {
        bail!("CSRF mismatch in OAuth callback");
    }

    let token_resp = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .context("notion token exchange")?;

    server.abort();

    let access = token_resp
        .access_token()
        .secret()
        .clone();

    // Notion's response includes workspace_id + workspace_name beyond the standard token fields.
    // The oauth2 crate ignores extras, so refetch via /v1/users/me to get workspace info.
    let client_http = Client::new();
    let me = client_http
        .get(format!("{NOTION_API_BASE}/users/me"))
        .bearer_auth(&access)
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await?;
    let me_json: serde_json::Value = me.json().await.unwrap_or_default();
    let workspace_name = me_json
        .get("bot")
        .and_then(|b| b.get("workspace_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let workspace_id = me_json
        .get("bot")
        .and_then(|b| b.get("workspace_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(OAuthResult {
        access_token: access,
        workspace_id,
        workspace_name,
    })
}
```

- [ ] **Step 2: Wire into `cmd/source_cmd.rs` `add_notion` handler**

Replace the `AddKind::Notion { .. } => bail!(...)` arm with a real call. At the bottom of source_cmd.rs:

```rust
async fn add_notion(
    instance: Option<String>,
    workspace: Option<String>,
    token: Option<String>,
) -> Result<()> {
    use crate::sources::adapters::notion::{OAuthResult, run_oauth_flow};
    use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE, account};
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("notion:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|s| s == "notion") {
                "notion".to_string()
            } else {
                let mut rng: u16 = rand::random();
                loop {
                    let candidate = format!("notion:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let (access_token, workspace_id, workspace_name) = if let Some(pat) = token {
        (pat, workspace, None::<String>)
    } else {
        println!("→ launching Notion OAuth (PKCE) flow…");
        let OAuthResult {
            access_token,
            workspace_id,
            workspace_name,
        } = run_oauth_flow().await?;
        (access_token, workspace_id, workspace_name)
    };

    // Persist credentials to keyring
    let keyring = OsKeyring;
    let kr_account = account(&id, "access_token");
    keyring
        .set(SERVICE, &kr_account, &access_token)
        .context("store notion access_token in keyring")?;

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    if let Some(w) = workspace_id {
        scope.insert(
            "workspace_id".into(),
            serde_yaml::Value::String(w),
        );
    }
    if let Some(n) = workspace_name {
        scope.insert(
            "workspace_name".into(),
            serde_yaml::Value::String(n.clone()),
        );
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "notion".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: Some(kr_account.clone()),
    };
    store.save(&inst)?;
    println!("✅ Connected Notion as `{id}`");
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}
```

In the `handle()` match, replace the AddKind::Notion arm:

```rust
            AddKind::Notion {
                instance,
                workspace,
                token,
            } => add_notion(instance, workspace, token).await,
```

- [ ] **Step 3: Update sync handler to construct NotionAdapter**

Find the `for mut inst in targets` loop in `sync()`. Currently it bails for non-obsidian. Add a notion arm:

```rust
        if inst.type_name == "notion" {
            use crate::sources::adapters::notion::NotionAdapter;
            use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE};
            let kr = OsKeyring;
            let kr_account = inst.keyring_entry.clone().unwrap_or_else(|| format!("{}:access_token", inst.id));
            let token = kr
                .get(SERVICE, &kr_account)?
                .ok_or_else(|| anyhow::anyhow!("no notion token in keyring for `{}`", inst.id))?;
            let adapter = NotionAdapter::from_instance(&inst, token)?;
            println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
            let report = sync_source(
                &adapter,
                &mut inst,
                &store,
                vector_store.clone(),
                &tantivy,
                &emb_cfg,
                full,
            )
            .await?;
            println!(
                "  synced {} docs ({} chunks), deleted {}, {} errors",
                report.docs_synced, report.chunks_emitted, report.docs_deleted, report.errors.len()
            );
            for e in report.errors.iter().take(3) {
                println!("  ! {e}");
            }
            continue;
        }
```

Place this BEFORE the existing `if inst.type_name != "obsidian"` check.

Same for joplin (next task does the same wiring).

- [ ] **Step 4: Compile + commit**

```bash
cargo check --workspace 2>&1 | tail -10
```

If `oauth2::reqwest::async_http_client` requires a feature flag, add `oauth2 = { version = "4", features = ["reqwest"] }` to Cargo.toml (Task 1 didn't specify; likely needed).

```bash
git add mur-core/Cargo.toml mur-core/src/sources/adapters/notion.rs mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): mur source add notion (PAT + PKCE OAuth) + sync wiring"
```

---

## Task 6: Joplin Server Mode

**Files:** `mur-core/src/sources/adapters/joplin.rs`, `cmd/source_cmd.rs`

- [ ] **Step 1: Extend `JoplinMode` enum + adapter**

In `joplin.rs`:

```rust
pub enum JoplinMode {
    LocalDb { db_path: PathBuf },
    Server { url: String, token: String },
}
```

Update `from_instance` to accept either mode (and update `KnowledgeSource` impl methods to dispatch):

```rust
    pub fn from_instance(instance: &SourceInstance, server_token: Option<String>) -> Result<Self> {
        if instance.type_name != "joplin" {
            bail!("expected type_name 'joplin', got '{}'", instance.type_name);
        }
        if let Some(server_url) = instance.scope.get("server_url").and_then(|v| v.as_str()) {
            let token = server_token.context("joplin server mode needs a token")?;
            return Ok(Self {
                id: instance.id.clone(),
                mode: JoplinMode::Server { url: server_url.to_string(), token },
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
```

Add server-mode branches in `list_documents` and `fetch`. Server REST API uses:
- `GET /api/items?type=note&token=<>&fields=id,title,updated_time` (paginated)
- `GET /api/items/<id>?token=<>&fields=body`

```rust
    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        match &self.mode {
            JoplinMode::LocalDb { .. } => self.list_local(cursor).await,
            JoplinMode::Server { url, token } => self.list_server(url, token, cursor).await,
        }
    }
```

Refactor existing local body into `list_local`, and add `list_server`:

```rust
    async fn list_server(
        &self,
        url: &str,
        token: &str,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        // Joplin Server pagination: ?cursor=<token>; we use updated_time filter
        let threshold_ms: Option<i64> = cursor.and_then(|c| {
            DateTime::parse_from_rfc3339(&c.0).ok().map(|dt| dt.timestamp_millis())
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
            let resp = client.get(&req_url).send().await.context("joplin server list")?;
            if !resp.status().is_success() {
                bail!("joplin server returned {}: {}", resp.status(), resp.text().await.unwrap_or_default());
            }
            let v: serde_json::Value = resp.json().await?;
            let items = v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
            if items.is_empty() {
                break;
            }
            for item in items {
                let id = item.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let title = item.get("title").and_then(|x| x.as_str()).map(|s| s.to_string());
                let updated_ms = item.get("updated_time").and_then(|x| x.as_i64()).unwrap_or(0);
                if let Some(t) = threshold_ms {
                    if updated_ms <= t {
                        continue;
                    }
                }
                if updated_ms > max_ms {
                    max_ms = updated_ms;
                }
                let updated_at = DateTime::<Utc>::from_timestamp_millis(updated_ms).unwrap_or_else(Utc::now);
                docs.push(DocRef { external_id: id, title, updated_at });
            }
            let has_more = v.get("has_more").and_then(|x| x.as_bool()).unwrap_or(false);
            if !has_more {
                break;
            }
            page += 1;
        }
        let cursor_out = if max_ms > 0 {
            SyncCursor(DateTime::<Utc>::from_timestamp_millis(max_ms).unwrap_or_else(Utc::now).to_rfc3339())
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
            bail!("joplin fetch failed {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string())
    }
```

Update `fetch` to dispatch:

```rust
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
        let title = doc_ref.title.clone().unwrap_or_else(|| doc_ref.external_id.clone());
        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::Markdown(body_text),
            url: Some(format!("joplin://x-callback-url/openNote?id={}", doc_ref.external_id)),
            updated_at: doc_ref.updated_at,
            tags: vec![],
            metadata: serde_json::Value::Null,
        })
    }
```

- [ ] **Step 2: Wire `add_joplin` in source_cmd.rs**

Add handler:

```rust
async fn add_joplin(
    instance: Option<String>,
    db: Option<std::path::PathBuf>,
    server: Option<String>,
    token: Option<String>,
) -> Result<()> {
    use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE, account};
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("joplin:{tag}"),
        _ => "joplin".to_string(),
    };

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    let mut keyring_entry: Option<String> = None;
    if let Some(srv) = server {
        let tok = token.context("joplin --server requires --token")?;
        scope.insert(
            "server_url".into(),
            serde_yaml::Value::String(srv),
        );
        let kr = OsKeyring;
        let kr_account = account(&id, "api_token");
        kr.set(SERVICE, &kr_account, &tok)?;
        keyring_entry = Some(kr_account);
    } else if let Some(db_path) = db {
        let abs = std::fs::canonicalize(&db_path)
            .with_context(|| format!("resolve {}", db_path.display()))?;
        scope.insert(
            "db_path".into(),
            serde_yaml::Value::String(abs.to_string_lossy().to_string()),
        );
    } else {
        bail!("specify --db <path> for local SQLite or --server <url> --token <pat> for Joplin Server");
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "joplin".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry,
    };
    store.save(&inst)?;
    println!("✅ Connected Joplin as `{id}`");
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}
```

Replace `AddKind::Joplin { .. } => bail!(...)` arm with `add_joplin(instance, db, server, token).await`.

Add a joplin sync arm in `sync()` (mirror the notion one):

```rust
        if inst.type_name == "joplin" {
            use crate::sources::adapters::joplin::JoplinAdapter;
            use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE};
            let token = if inst.scope.get("server_url").is_some() {
                let kr = OsKeyring;
                let kr_account = inst.keyring_entry.clone().unwrap_or_else(|| format!("{}:api_token", inst.id));
                Some(kr.get(SERVICE, &kr_account)?.ok_or_else(|| anyhow::anyhow!("no joplin token for `{}`", inst.id))?)
            } else {
                None
            };
            let adapter = JoplinAdapter::from_instance(&inst, token)?;
            println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
            let report = sync_source(
                &adapter, &mut inst, &store, vector_store.clone(), &tantivy, &emb_cfg, full,
            ).await?;
            println!(
                "  synced {} docs ({} chunks), deleted {}, {} errors",
                report.docs_synced, report.chunks_emitted, report.docs_deleted, report.errors.len()
            );
            continue;
        }
```

- [ ] **Step 3: Compile + commit**

```bash
cargo check --workspace 2>&1 | tail -5
git add mur-core/src/sources/adapters/joplin.rs mur-core/src/cmd/source_cmd.rs
git commit -m "feat(joplin): server mode + add joplin handler + sync wiring"
```

---

## Task 7: Watch Mode (file watcher + cloud polling)

**Files:** `mur-core/src/sources/watch.rs`, `cmd/source_cmd.rs`

- [ ] **Step 1: Create `watch.rs`**

```rust
//! `mur source sync --watch` orchestrator.
//!
//! Combines:
//!   - `notify` file-watcher events for local-file adapters (Obsidian, Joplin local)
//!   - `tokio::time::interval` polling for cloud adapters (Notion, Joplin Server)
//!
//! Foreground daemon — Ctrl+C exits cleanly.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::sources::instance::{SourceInstance, SourceInstanceStore};
use crate::sources::tantivy::TantivyIndex;
use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::VectorStore;

pub struct WatchOptions {
    pub poll_interval_secs: u64,
}

/// Run watch mode. Loops forever (until SIGINT). Logs each sync via tracing.
pub async fn run_watch(
    instance_store: SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    tantivy: TantivyIndex,
    embedding_cfg: EmbeddingConfig,
    opts: WatchOptions,
) -> Result<()> {
    let instances = instance_store.list()?;
    if instances.is_empty() {
        println!("(no sources to watch)");
        return Ok(());
    }
    println!(
        "🔭 watching {} source(s); poll interval = {}s; Ctrl+C to stop",
        instances.len(),
        opts.poll_interval_secs
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<String>(); // source_id needing sync

    // Spawn a debouncer for each Obsidian vault.
    let mut watcher_handles: Vec<notify::RecommendedWatcher> = Vec::new();
    for inst in &instances {
        if inst.type_name == "obsidian" && inst.enabled {
            if let Some(vault) = inst.scope.get("vault").and_then(|v| v.as_str()) {
                let vp = PathBuf::from(vault);
                let id = inst.id.clone();
                let tx_clone = tx.clone();
                use notify::{Event, RecursiveMode, Watcher};
                let mut w = notify::recommended_watcher(move |res: notify::Result<Event>| {
                    if let Ok(ev) = res {
                        let touches_md = ev.paths.iter().any(|p| p.extension().is_some_and(|e| e == "md"));
                        if touches_md {
                            let _ = tx_clone.send(id.clone());
                        }
                    }
                })
                .context("create file watcher")?;
                w.watch(&vp, RecursiveMode::Recursive)
                    .with_context(|| format!("watch {}", vp.display()))?;
                watcher_handles.push(w);
            }
        }
    }

    // Cloud-poll ticker.
    let mut poll = tokio::time::interval(Duration::from_secs(opts.poll_interval_secs));
    poll.tick().await; // skip first immediate tick

    let cloud_ids: Vec<String> = instances
        .iter()
        .filter(|i| i.enabled && (i.type_name == "notion" || (i.type_name == "joplin" && i.scope.get("server_url").is_some())))
        .map(|i| i.id.clone())
        .collect();

    // Debouncer: collapse rapid file events on the same source within 500ms.
    let mut last_sent: std::collections::HashMap<String, std::time::Instant> = std::collections::HashMap::new();

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                println!("\n🛑 received Ctrl+C, exiting");
                break;
            }
            _ = poll.tick() => {
                for id in &cloud_ids {
                    let _ = tx.send(id.clone());
                }
            }
            Some(src_id) = rx.recv() => {
                let now = std::time::Instant::now();
                if let Some(prev) = last_sent.get(&src_id) {
                    if now.duration_since(*prev) < Duration::from_millis(500) {
                        continue;
                    }
                }
                last_sent.insert(src_id.clone(), now);
                tracing::info!(source = %src_id, "watch: triggering sync");
                if let Err(e) = sync_one(
                    &src_id,
                    &instance_store,
                    vector_store.clone(),
                    &tantivy,
                    &embedding_cfg,
                ).await {
                    tracing::warn!(source = %src_id, error = %e, "watch: sync failed");
                }
            }
        }
    }
    drop(watcher_handles);
    Ok(())
}

async fn sync_one(
    source_id: &str,
    instance_store: &SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    tantivy: &TantivyIndex,
    embedding_cfg: &EmbeddingConfig,
) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::sync::sync_source;

    let mut inst: SourceInstance = instance_store.load(source_id)?;
    // Watch mode is incremental — `full = false` so we don't re-embed everything per file change.
    match inst.type_name.as_str() {
        "obsidian" => {
            let adapter = ObsidianAdapter::from_instance(&inst)?;
            sync_source(
                &adapter, &mut inst, instance_store, vector_store, tantivy, embedding_cfg, false,
            )
            .await?;
        }
        "notion" => {
            use crate::sources::adapters::notion::NotionAdapter;
            use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE};
            let kr = OsKeyring;
            let kr_account = inst.keyring_entry.clone().unwrap_or_else(|| format!("{}:access_token", inst.id));
            let token = kr.get(SERVICE, &kr_account)?
                .ok_or_else(|| anyhow::anyhow!("no notion token in keyring for `{}`", inst.id))?;
            let adapter = NotionAdapter::from_instance(&inst, token)?;
            sync_source(
                &adapter, &mut inst, instance_store, vector_store, tantivy, embedding_cfg, false,
            )
            .await?;
        }
        "joplin" => {
            use crate::sources::adapters::joplin::JoplinAdapter;
            use crate::sources::credentials::{OsKeyring, CredentialStore, SERVICE};
            let token = if inst.scope.get("server_url").is_some() {
                let kr = OsKeyring;
                let kr_account = inst.keyring_entry.clone().unwrap_or_else(|| format!("{}:api_token", inst.id));
                Some(kr.get(SERVICE, &kr_account)?.ok_or_else(|| anyhow::anyhow!("no joplin token"))?)
            } else {
                None
            };
            let adapter = JoplinAdapter::from_instance(&inst, token)?;
            sync_source(
                &adapter, &mut inst, instance_store, vector_store, tantivy, embedding_cfg, false,
            )
            .await?;
        }
        other => {
            anyhow::bail!("watch: unsupported adapter type `{other}`");
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Register module**

Append to `mur-core/src/sources/mod.rs`:

```rust
pub mod watch;
```

- [ ] **Step 3: Wire `--watch` in `source_cmd.rs`**

Find the `if watch { bail!("`mur source sync --watch` arrives in P1.4"); }` line. Replace with:

```rust
        SourceCommand::Sync { id, full, watch } => {
            if watch {
                sync_watch().await
            } else {
                sync(id.as_deref(), full).await
            }
        }
```

Add `sync_watch` handler:

```rust
async fn sync_watch() -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::tantivy::TantivyIndex;
    use crate::sources::watch::{WatchOptions, run_watch};
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir().context("no home dir")?.join(".mur").join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy = TantivyIndex::open_or_create(&dirs::home_dir().unwrap().join(".mur"))?;
    let instance_store = SourceInstanceStore::default_store()?;
    run_watch(
        instance_store,
        vector_store,
        tantivy,
        emb_cfg,
        WatchOptions {
            poll_interval_secs: cfg.sources_global.poll_interval_secs,
        },
    )
    .await
}
```

- [ ] **Step 4: Compile + commit**

```bash
cargo check --workspace 2>&1 | tail -5
git add mur-core/src/sources/mod.rs mur-core/src/sources/watch.rs mur-core/src/cmd/source_cmd.rs
git commit -m "feat(sources/watch): fsevents + cloud-poll orchestrator (sync --watch)"
```

---

## Task 8: `install-schedule` Command

**Files:** `mur-core/src/cmd/source_cmd.rs`

- [ ] **Step 1: Add the handler**

```rust
async fn install_schedule() -> Result<()> {
    use anyhow::Context;
    use std::io::Write;

    let cfg = crate::store::config::load_config()?;
    let interval_secs = cfg.sources_global.poll_interval_secs;

    let mur_path = std::env::current_exe().context("locate mur binary")?;
    let mur_path_str = mur_path.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>run.mur.source-sync</string>
  <key>ProgramArguments</key>
  <array>
    <string>{mur_path_str}</string>
    <string>source</string>
    <string>sync</string>
  </array>
  <key>StartInterval</key><integer>{interval_secs}</integer>
  <key>StandardOutPath</key><string>/tmp/mur-source-sync.log</string>
  <key>StandardErrorPath</key><string>/tmp/mur-source-sync.err</string>
</dict>
</plist>
"#
        );
        let path = dirs::home_dir()
            .context("no home dir")?
            .join("Library/LaunchAgents/run.mur.source-sync.plist");
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::File::create(&path)?;
        f.write_all(plist.as_bytes())?;
        println!("✅ wrote {}", path.display());
        println!("Enable with: launchctl load -w {}", path.display());
        println!("Disable with: launchctl unload {}", path.display());
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let svc_dir = dirs::config_dir().context("no config dir")?.join("systemd/user");
        std::fs::create_dir_all(&svc_dir)?;
        let svc_file = svc_dir.join("mur-source-sync.service");
        let timer_file = svc_dir.join("mur-source-sync.timer");

        let svc = format!(
            "[Unit]\nDescription=mur source sync\n\n[Service]\nType=oneshot\nExecStart={mur_path_str} source sync\n"
        );
        let timer = format!(
            "[Unit]\nDescription=Run mur source sync periodically\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec={}s\nUnit=mur-source-sync.service\n\n[Install]\nWantedBy=timers.target\n",
            interval_secs
        );
        std::fs::write(&svc_file, svc)?;
        std::fs::write(&timer_file, timer)?;
        println!("✅ wrote {} and {}", svc_file.display(), timer_file.display());
        println!("Enable with: systemctl --user enable --now mur-source-sync.timer");
        println!("Disable with: systemctl --user disable --now mur-source-sync.timer");
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("install-schedule supported on macOS/Linux only");
    }
}
```

Replace `SourceCommand::InstallSchedule => bail!(...)` with:

```rust
        SourceCommand::InstallSchedule => install_schedule().await,
```

- [ ] **Step 2: Compile + commit**

```bash
cargo check --workspace 2>&1 | tail -5
git add mur-core/src/cmd/source_cmd.rs
git commit -m "feat(cli): mur source install-schedule (launchd / systemd unit gen)"
```

---

## Task 9: Wire `format_notes_section` into Inject Hook

**Files:** `mur-core/src/inject/hook.rs`

- [ ] **Step 1: Find the entry**

```bash
grep -n "pub fn build_context\|pub async fn build_context\|fn run_inject\|format_pattern_section\|inject_for_query" /Volumes/Firecuda4tb/Projects/mur/.worktrees/sources-p1.4/mur-core/src/inject/hook.rs | head -10
```

Identify the function that constructs the final injected text (most likely a function that returns `String` containing patterns). At its END, append a call to `format_notes_section(&note_hits)` and concatenate.

- [ ] **Step 2: Add the source-fetch helper inside hook.rs**

```rust
#[cfg(feature = "sources")]
async fn fetch_source_hits_for_query(query: &str, k: usize) -> Vec<crate::store::vector::Hit> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::{SearchFilter, factory::get_vector_store};

    let Ok(cfg) = crate::store::config::load_config() else { return vec![] };
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let Some(home) = dirs::home_dir() else { return vec![] };
    let index_path = home.join(".mur").join("index");
    let Ok(vs) = get_vector_store(&cfg, &index_path).await else { return vec![] };
    let Ok(tantivy) = crate::sources::tantivy::TantivyIndex::open_or_create(&home.join(".mur")) else { return vec![] };
    let weights: std::collections::HashMap<String, f32> = crate::sources::instance::SourceInstanceStore::default_store()
        .and_then(|s| s.list())
        .map(|v| v.into_iter().map(|i| (i.id, i.weight)).collect())
        .unwrap_or_default();
    let Ok(qvec) = embed(query, &emb_cfg).await else { return vec![] };
    let filter = SearchFilter::default();
    let Ok(unified) = crate::retrieve::retrieve_unified(query, vs, &tantivy, &emb_cfg, &weights, &filter, 0, k, 0.35).await else { return vec![] };
    unified.into_iter().map(|u| u.hit).collect()
}
```

- [ ] **Step 3: Append in the entry**

In whatever function builds the final text (call it `build_inject_text`), at the bottom (after pattern section is appended), add:

```rust
    #[cfg(feature = "sources")]
    {
        let note_hits = fetch_source_hits_for_query(query, 3).await;
        let notes_section = format_notes_section(&note_hits);
        out.push_str(&notes_section);
    }
```

(`query` and `out` are placeholder names; use whatever the actual function's variables are.)

If the inject entry is synchronous (no `async`), spawn a `tokio::runtime::Handle::current().block_on(...)` is NOT recommended. Instead, change the entry to `async` if it isn't already, OR add the source query as a separate hook that's called by the same shell pipeline.

If the analysis reveals integration is more invasive than expected, document the work as PARTIAL and commit a follow-up plan note. The `format_notes_section` function exists and is ready; the wire-up is judgment-dependent.

- [ ] **Step 4: Compile + smoke + commit**

```bash
cargo check --workspace 2>&1 | tail -5
git add mur-core/src/inject/hook.rs
git commit -m "feat(inject): append Notes section to injected context (sources feature)"
```

If the integration is ultimately deferred for scope reasons, commit just the helper and document:

```bash
git commit -m "feat(inject): fetch_source_hits_for_query helper (wiring deferred)

The format_notes_section + fetch helper are in place. Wiring into the
top-level inject entry depends on the existing entry's async/sync shape
and varies per call site. Deferred to a follow-up that does the
audit and wiring task-by-task."
```

---

## Task 10: docs/source-adapters.md (User Setup Notes)

**Files:** `docs/source-adapters.md`

- [ ] **Step 1: Write user-facing setup notes**

```markdown
# External Sources Setup

## Obsidian
```
mur source add obsidian --vault ~/Documents/MyVault
```
No auth required. mur reads `*.md` files; ignores `.obsidian/` and `.trash/`. Use `--exclude-folder Drafts,Sandbox` to skip more.

## Notion
Two paths:

### A. Internal Integration Token (PAT) — fastest
1. Visit https://www.notion.so/my-integrations and create an internal integration.
2. Copy the "Internal Integration Token".
3. In Notion, share the pages/databases you want indexed with your integration ("Add connections" in the page menu).
4. Run:
   ```
   mur source add notion --token <pat>
   ```

### B. Public OAuth (PKCE) — for shared installs
Build mur with your `MUR_NOTION_CLIENT_ID` env var set. Then:
```
mur source add notion
```
Browser opens; authorize the workspace. Token is stored in OS keyring (macOS Keychain, Linux Secret Service, Windows Credential Manager).

## Joplin
Two modes:

### Local SQLite (single-machine)
```
mur source add joplin --db ~/Library/Application\ Support/joplin-desktop/database.sqlite
```
(Linux: `~/.config/joplin-desktop/database.sqlite`)

### Joplin Server (multi-device)
```
mur source add joplin --server https://your-joplin-server --token <api-token>
```

## Sync
- Manual: `mur source sync [<id>] [--full]`
- Watch (foreground daemon): `mur source sync --watch`
- Scheduled: `mur source install-schedule` then enable per the printed launchctl/systemctl command

## Search
- `mur search "query"` returns Patterns + Notes sections
- `mur search "query" --only-sources` for sources only
- `mur source search "query" -k 5` direct sources query
```

- [ ] **Step 2: Commit**

```bash
git add docs/source-adapters.md
git commit -m "docs: external-sources user setup notes"
```

---

## Task 11: Final Verification

- [ ] **Step 1: Workspace tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result:"
```

All ok, 0 failed.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-features -- -D warnings 2>&1 | tail -20
```

Fix only P1.4-introduced warnings.

- [ ] **Step 3: Fmt**

```bash
cargo fmt --check && echo clean || (cargo fmt && git add -A && git commit -m "style: cargo fmt after P1.4")
```

- [ ] **Step 4: Feature matrix**

```bash
cargo build --workspace 2>&1 | tail -3
cargo build --workspace --no-default-features --features "cli server" 2>&1 | tail -3
```

Both must succeed.

- [ ] **Step 5: CLAUDE.md update**

Find the `**Sources pipeline (P1.3 — ...)**` line. Replace with:

```
**Sources pipeline (P1.4 — All adapters shipped: Obsidian + Notion + Joplin; --watch + install-schedule; format_notes_section ready):**
```

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): mark P1.4 complete (Notion + Joplin + watch + schedule)"
```

## Done Criteria (P1.4)

- [ ] Notion block-to-markdown chunker with 6 unit tests
- [ ] NotionAdapter (PAT + OAuth/PKCE) implements `KnowledgeSource`
- [ ] JoplinAdapter (Local SQLite + Server) implements `KnowledgeSource`
- [ ] `mur source add notion [--token <pat>] [--workspace <id>]` works
- [ ] `mur source add joplin --db <path>` and `--server <url> --token <pat>` work
- [ ] `mur source sync --watch` foreground daemon (fsevents + poll)
- [ ] `mur source install-schedule` writes launchd plist (macOS) / systemd unit (Linux)
- [ ] `inject::format_notes_section` is wired into the inject entry (or follow-up plan filed)
- [ ] `docs/source-adapters.md` published
- [ ] Tests + clippy + fmt + feature matrix all green
- [ ] CLAUDE.md updated

**Out of scope for P1.4** (truly final wishlist):
- Notion database property pages (only body blocks indexed)
- Inline `#tag` parsing in Obsidian (only YAML frontmatter `tags:` covered)
- Cross-encoder reranking, query expansion, personalisation
- Web UI for sources management
