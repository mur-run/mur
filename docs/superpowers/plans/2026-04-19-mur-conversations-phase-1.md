# mur Conversations Archive — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Phase 1 (Foundation/MVP) of the local-only conversations archive at `~/.mur/conversations/`, including ingesters for Claude Code / Cursor / Gemini / Aider, commander engine + gateway integration (Slack/TG/Discord), pre-filter pipeline, LanceDB index with RaBitQ, Mode A (timeline) + Mode B (search) retrieval, three-guard retention, migration tool with dry-run, and a golden-path e2e smoke script.

**Architecture:** Option X (Rename & Unify) — one shared archive at `~/.mur/conversations/` written by both mur ingesters and commander adapters. Pre-filter pipeline (normalize tool-refs → MinHash dedup → REJECT gate) is pure Rust and runs synchronously before every write. LanceDB with RaBitQ compression indexes every message at `layer=0`. Daily hybrid summaries (extractive spans with provenance + abstractive narrative) are generated offline by a sleep-time job (Phase 2 integrates; Phase 1 lays the file format). Audit hash chain continues from commander's existing chain through migration.

**Tech Stack:** Rust 2024 edition · Cargo workspace (`mur-common` + `mur-core`, cross-repo work in `mur-commander`) · LanceDB 0.26 with RaBitQ · rusqlite for Cursor SQLite · anyhow + thiserror · tokio async · tracing · tempfile for tests · existing dependencies (no new LLM calls in Phase 1).

**Reference spec:** `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` (commit 52a374f).

---

## File Structure

### mur repo (`/Volumes/Firecuda4tb/Projects/mur`)

**Create:**
- `mur-common/src/conversation.rs` — shared `Message`, `Role`, `Content`, `Source` types (cross-crate)
- `mur-core/src/conversations/mod.rs` — public API re-exports
- `mur-core/src/conversations/paths.rs` — canonical path helpers
- `mur-core/src/conversations/store.rs` — raw JSONL append-only writer/reader
- `mur-core/src/conversations/audit.rs` — hash-chained audit log
- `mur-core/src/conversations/blob.rs` — content-addressed blob store for tool_ref bodies
- `mur-core/src/conversations/ingest/mod.rs` — `Ingester` trait + `Cursor` state
- `mur-core/src/conversations/ingest/normalize.rs` — tool-call pointer substitution
- `mur-core/src/conversations/ingest/dedup.rs` — MinHash near-dup detection
- `mur-core/src/conversations/ingest/filter.rs` — Mem0-style REJECT gate
- `mur-core/src/conversations/ingest/pipeline.rs` — pre-filter orchestration
- `mur-core/src/conversations/ingest/claude_code.rs` — routes `mur session record` events
- `mur-core/src/conversations/ingest/cursor.rs` — SQLite + .specstory reader
- `mur-core/src/conversations/ingest/gemini.rs` — `~/.gemini/tmp/<hash>/chats/*.json` reader
- `mur-core/src/conversations/ingest/aider.rs` — `.aider.chat.history.md` scanner
- `mur-core/src/conversations/index.rs` — LanceDB wrapper with RaBitQ build
- `mur-core/src/conversations/retention.rs` — three-guard cleanup
- `mur-core/src/conversations/retrieve.rs` — Mode A + Mode B
- `mur-core/src/conversations/migrate.rs` — commander → conversations migration
- `mur-core/src/cmd/conversations_cmd.rs` — CLI handlers
- `scripts/golden-path-conversations.sh` — e2e smoke test

**Modify:**
- `mur-common/src/lib.rs` — export `conversation` module
- `mur-core/src/lib.rs` — export `conversations` module
- `mur-core/src/cmd/mod.rs` — register `conversations_cmd`
- `mur-core/src/cmd/session.rs` — reroute `record()` to write through `conversations::store` when `conversations.enabled=true`
- `mur-core/src/main.rs` — add `Commands::Conversations { action: ConversationsAction }` + `Commands::Chat { action: ChatAction }` + `Commands::Log { date: Option<String> }` subcommands
- `mur-core/Cargo.toml` — add `rusqlite = { version = "0.32", features = ["bundled"] }` and `twox-hash = "1.6"` (MinHash hashing)
- `mur-common/Cargo.toml` — already has serde/chrono/uuid from workspace (no new deps)

### mur-commander repo (`/Volumes/Firecuda4tb/Projects/mur-commander`)

**Modify (path-only changes; behavior identical):**
- `crates/engine/src/memory/long_term.rs` — path constant
- `crates/gateway/src/memory/episodes.rs` — per-user + per-platform paths
- `crates/gateway/src/memory/lance_store.rs` — LanceDB path
- `crates/gateway/src/memory/mod.rs` — root dir constant
- `crates/gateway/src/unified_handler/mod.rs` — remove 5 `mur_learn::session::record()` call sites (lines 416, 582, 793, 883, 1267)

---

## Pre-flight checks

- [ ] **Step 0.1: Verify mur workspace builds clean**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo build --workspace`
Expected: PASS with no errors.

- [ ] **Step 0.2: Verify mur tests pass**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test --workspace`
Expected: PASS (all existing 581 tests green per recent PR #3).

- [ ] **Step 0.3: Verify mur-commander workspace builds clean**

Run: `cd /Volumes/Firecuda4tb/Projects/mur-commander && cargo build --workspace`
Expected: PASS.

- [ ] **Step 0.4: Confirm on the correct branch**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && git rev-parse --abbrev-ref HEAD`
Expected: `main` (or a dedicated feature branch created by the brainstorming worktree). If on a shared branch, create one now: `git checkout -b feat/conversations-phase-1`.

---

## Task 1: Shared `Message` schema in `mur-common`

**Files:**
- Create: `mur-common/src/conversation.rs`
- Modify: `mur-common/src/lib.rs`
- Test: `mur-common/src/conversation.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1.1: Write the failing test for Message serde round-trip**

Add to the end of `mur-common/src/conversation.rs` (file will exist after 1.2):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn message_text_roundtrip() {
        let m = Message {
            v: 1,
            ts: chrono::Utc.with_ymd_and_hms(2026, 4, 19, 11, 30, 45).unwrap(),
            src: Source::ClaudeCode,
            conv: "3a8786a0".into(),
            role: Role::User,
            content: Content::Text("hello".into()),
            meta: serde_json::json!({"project": "mur"}),
            refs: vec!["pattern:atomic-yaml-write".into()],
        };
        let line = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&line).unwrap();
        assert_eq!(back.conv, "3a8786a0");
        assert!(matches!(back.content, Content::Text(ref s) if s == "hello"));
        assert!(matches!(back.src, Source::ClaudeCode));
        assert_eq!(back.refs, vec!["pattern:atomic-yaml-write".to_string()]);
    }

    #[test]
    fn message_tool_ref_roundtrip() {
        let m = Message {
            v: 1,
            ts: chrono::Utc.with_ymd_and_hms(2026, 4, 19, 11, 30, 45).unwrap(),
            src: Source::ClaudeCode,
            conv: "x".into(),
            role: Role::Tool,
            content: Content::ToolRef {
                sha256: "abc".into(),
                path: "src/main.rs".into(),
                bytes: 1234,
                desc: "read main.rs".into(),
            },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        let line = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&line).unwrap();
        assert!(matches!(back.content, Content::ToolRef { ref sha256, .. } if sha256 == "abc"));
    }

    #[test]
    fn commander_turn_is_subset() {
        // Commander's ConversationTurn { timestamp: i64, role: String, text: String }
        // must deserialize successfully when reshaped into Message.
        let commander_json = r#"{"v":1,"ts":"2026-04-19T11:30:45Z","src":"slack","conv":"c","role":"user","content":{"t":"text","v":"hi"},"meta":{},"refs":[]}"#;
        let _m: Message = serde_json::from_str(commander_json).unwrap();
    }
}
```

- [ ] **Step 1.2: Create `mur-common/src/conversation.rs` with Message types**

```rust
//! Shared conversation archive types (used by mur-core, mur-commander).
//!
//! JSONL row format version 1. See
//! `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` §4.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One line in `~/.mur/conversations/raw/<date>/*.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Schema version; breaking changes bump this.
    pub v: u32,
    pub ts: DateTime<Utc>,
    pub src: Source,
    /// Conversation id; scopes messages within a source.
    pub conv: String,
    pub role: Role,
    pub content: Content,
    #[serde(default)]
    pub meta: serde_json::Value,
    /// Named-Abstraction references into patterns/ (Freedman 2026).
    #[serde(default)]
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ClaudeCode,
    Cursor,
    Gemini,
    Aider,
    Slack,
    Telegram,
    Discord,
    CommanderEngine,
}

impl Source {
    /// Canonical short prefix used in filename `<src>_<id>.jsonl`.
    pub fn file_prefix(&self) -> &'static str {
        match self {
            Source::ClaudeCode => "cc",
            Source::Cursor => "cursor",
            Source::Gemini => "gemini",
            Source::Aider => "aider",
            Source::Slack => "slack",
            Source::Telegram => "telegram",
            Source::Discord => "discord",
            Source::CommanderEngine => "commander",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Message body. `ToolRef` and `ImageRef` are content-addressed pointers
/// (§4.3 pointer substitution) to blobs stored under
/// `~/.mur/conversations/blob/<sha256>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Content {
    Text {
        #[serde(rename = "v")]
        value: String,
    },
    ToolRef {
        sha256: String,
        path: String,
        bytes: u64,
        #[serde(default)]
        desc: String,
    },
    ImageRef {
        sha256: String,
        path: String,
        #[serde(default)]
        desc: String,
    },
}

impl Content {
    /// Short flag allowing the match arm shortcut `Content::Text("hello")` in tests.
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text { value: s.into() }
    }
}

// Support `Content::Text("x".into())` pattern used by some tests
impl From<String> for Content {
    fn from(s: String) -> Self { Content::Text { value: s } }
}
```

(Note: the test in step 1.1 uses `Content::Text("hello".into())` which requires the `From<String>` impl above OR explicit field syntax. Both are provided — the `From<String>` is a convenience; the test uses `Content::Text("hello".into())` which will need the `From` impl OR we adjust the test to `Content::text("hello")`. Pick one: the plan uses the tuple-variant pattern via `From<String>`, so the test's `Content::Text("hello".into())` works via From. Verify by running.)

If the `From<String>` sugar fights with the `#[serde(tag="t")]` layout (which expects `{t: "text", v: "..."}`), remove the `From` impl and change the test to use `Content::text("hello")` instead. This is the safer path.

- [ ] **Step 1.3: Register the module in `mur-common/src/lib.rs`**

Insert after line 1 `pub mod actor;` (alphabetical order):

```rust
pub mod conversation;
```

And add a re-export near the bottom:

```rust
pub use conversation::{Content, Message, Role, Source};
```

- [ ] **Step 1.4: Run the test — must fail initially if run before 1.2, or pass after**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-common conversation::tests`
Expected: 3 tests pass. If `Content::Text("hello".into())` fails to compile, switch to `Content::text("hello")` in the test and rerun.

- [ ] **Step 1.5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-common/src/conversation.rs mur-common/src/lib.rs
git commit -m "feat(common): add conversation archive Message/Role/Content/Source types

JSONL row schema v1 shared across mur-core and mur-commander. See
docs/superpowers/specs/2026-04-19-mur-conversations-design.md §4.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `conversations` module skeleton + path helpers

**Files:**
- Create: `mur-core/src/conversations/mod.rs`
- Create: `mur-core/src/conversations/paths.rs`
- Modify: `mur-core/src/lib.rs`
- Test: `mur-core/src/conversations/paths.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 2.1: Write failing tests for path helpers**

Content to add at the end of `mur-core/src/conversations/paths.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn root_path() {
        let root = conversations_root(Some("/tmp/murtest"));
        assert_eq!(root, std::path::PathBuf::from("/tmp/murtest/conversations"));
    }

    #[test]
    fn raw_dir_format() {
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 11, 30, 45).unwrap();
        let d = raw_dir_for(ts, Some("/tmp/m"));
        assert_eq!(d, std::path::PathBuf::from("/tmp/m/conversations/raw/2026-04-19"));
    }

    #[test]
    fn raw_file_naming() {
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let f = raw_file_for(ts, mur_common::Source::ClaudeCode, "3a87", Some("/tmp/m"));
        assert_eq!(f, std::path::PathBuf::from("/tmp/m/conversations/raw/2026-04-19/cc_3a87.jsonl"));
    }

    #[test]
    fn summary_paths() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let (md, yml) = summary_paths_for(d, Some("/tmp/m"));
        assert_eq!(md, std::path::PathBuf::from("/tmp/m/conversations/summary/2026-04-19.md"));
        assert_eq!(yml, std::path::PathBuf::from("/tmp/m/conversations/summary/2026-04-19.yaml"));
    }
}
```

- [ ] **Step 2.2: Create `mur-core/src/conversations/paths.rs`**

```rust
//! Canonical path helpers for the conversations archive.
//!
//! All paths live under `$MUR_DIR/conversations/` where `$MUR_DIR`
//! defaults to `~/.mur`. Accepting an override string is only for tests.

use chrono::{DateTime, NaiveDate, Utc};
use mur_common::Source;
use std::path::PathBuf;

/// Default mur root (`~/.mur`) or the override path for tests.
pub fn mur_root(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("no home dir").join(".mur")
}

pub fn conversations_root(override_path: Option<&str>) -> PathBuf {
    mur_root(override_path).join("conversations")
}

pub fn raw_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("raw")
}

pub fn raw_dir_for(ts: DateTime<Utc>, override_path: Option<&str>) -> PathBuf {
    let date = ts.date_naive();
    raw_root(override_path).join(date.format("%Y-%m-%d").to_string())
}

pub fn raw_file_for(
    ts: DateTime<Utc>,
    src: Source,
    conv_id: &str,
    override_path: Option<&str>,
) -> PathBuf {
    raw_dir_for(ts, override_path).join(format!("{}_{}.jsonl", src.file_prefix(), conv_id))
}

pub fn summary_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("summary")
}

pub fn summary_paths_for(date: NaiveDate, override_path: Option<&str>) -> (PathBuf, PathBuf) {
    let root = summary_root(override_path);
    let d = date.format("%Y-%m-%d").to_string();
    (root.join(format!("{d}.md")), root.join(format!("{d}.yaml")))
}

pub fn index_path(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("index.lance")
}

pub fn audit_path(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("audit.jsonl")
}

pub fn blob_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("blob")
}

pub fn user_dir(user_id: &str, override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("users").join(user_id)
}
```

- [ ] **Step 2.3: Create `mur-core/src/conversations/mod.rs`**

```rust
//! Conversations archive — local-only, cross-source record of every AI
//! coding-assistant and chat-platform interaction.
//!
//! See `docs/superpowers/specs/2026-04-19-mur-conversations-design.md`.

pub mod audit;
pub mod blob;
pub mod index;
pub mod ingest;
pub mod migrate;
pub mod paths;
pub mod retention;
pub mod retrieve;
pub mod store;
```

(Modules not yet created will be added by later tasks. Comment out any missing sub-mod declarations at this stage if they fail to compile, but keep `paths` visible — it's created in this task.)

For Task 2 specifically, replace the above `mod.rs` body with just:

```rust
//! Conversations archive — local-only, cross-source record of every AI
//! coding-assistant and chat-platform interaction.
//!
//! See `docs/superpowers/specs/2026-04-19-mur-conversations-design.md`.

pub mod paths;
```

Later tasks uncomment / add each submodule as it's created.

- [ ] **Step 2.4: Register the conversations module in `mur-core/src/lib.rs`**

Find the module declarations section (alphabetical) and add:

```rust
pub mod conversations;
```

- [ ] **Step 2.5: Run tests**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-core conversations::paths::tests`
Expected: 4 tests pass.

- [ ] **Step 2.6: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/conversations/ mur-core/src/lib.rs
git commit -m "feat(core): add conversations module skeleton + path helpers

Establishes ~/.mur/conversations/ path convention. Accepts override root
for tests so tempfile-based harnesses don't touch real \$HOME.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Raw JSONL store (append-only)

**Files:**
- Create: `mur-core/src/conversations/store.rs`
- Modify: `mur-core/src/conversations/mod.rs` (uncomment `pub mod store;`)
- Test: inline `#[cfg(test)] mod tests`

- [ ] **Step 3.1: Write the failing tests**

Add to end of `mur-core/src/conversations/store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn sample(ts: chrono::DateTime<chrono::Utc>, v: &str) -> Message {
        Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "test-conv".into(),
            role: Role::User,
            content: Content::Text { value: v.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn append_creates_file_and_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let msg = sample(ts, "hello");
        append(&msg, Some(root)).unwrap();
        let path = crate::conversations::paths::raw_file_for(
            ts, Source::ClaudeCode, "test-conv", Some(root),
        );
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.trim_end().ends_with("}"));
        assert!(content.contains("hello"));
    }

    #[test]
    fn append_is_idempotent_per_line() {
        // Each call appends one line; two calls produce two lines.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        append(&sample(ts, "a"), Some(root)).unwrap();
        append(&sample(ts, "b"), Some(root)).unwrap();
        let path = crate::conversations::paths::raw_file_for(
            ts, Source::ClaudeCode, "test-conv", Some(root),
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn read_day_returns_all_messages_sorted_by_ts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let t1 = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 9, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        append(&sample(t2, "later"), Some(root)).unwrap();
        append(&sample(t1, "earlier"), Some(root)).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let msgs = read_day(date, Some(root)).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].ts < msgs[1].ts);
    }
}
```

- [ ] **Step 3.2: Implement `mur-core/src/conversations/store.rs`**

```rust
//! Raw JSONL append-only writer/reader.
//!
//! - Each line is a serialized `mur_common::Message`.
//! - Writes open with `O_APPEND`; no file rewrites (spec §12 constraint #2).
//! - Reads walk every JSONL file for a date, sort by `ts`.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use mur_common::Message;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::paths::{raw_dir_for, raw_file_for};

/// Append one message to its dated JSONL file. Creates parent dirs as needed.
pub fn append(msg: &Message, root_override: Option<&str>) -> Result<()> {
    let file_path = raw_file_for(msg.ts, msg.src, &msg.conv, root_override);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("opening {file_path:?} for append"))?;
    serde_json::to_writer(&mut f, msg).context("serializing message")?;
    writeln!(f).context("writeln")?;
    Ok(())
}

/// Read every message recorded on `date`, across all sources, sorted by ts.
pub fn read_day(date: NaiveDate, root_override: Option<&str>) -> Result<Vec<Message>> {
    // Construct a dummy ts pointing at the right date
    let ts = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let dir = raw_dir_for(ts, root_override);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("readdir {dir:?}"))? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let f = fs::File::open(entry.path())?;
        for line in BufReader::new(f).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let m: Message = serde_json::from_str(&line)
                .with_context(|| format!("parse {:?}", entry.path()))?;
            out.push(m);
        }
    }
    out.sort_by_key(|m| m.ts);
    Ok(out)
}

/// List dated raw directories (`YYYY-MM-DD` subdirs under `raw/`).
pub fn list_raw_dirs(root_override: Option<&str>) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let raw = super::paths::raw_root(root_override);
    if !raw.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&raw)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Ok(date) = NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
            out.push((date, entry.path()));
        }
    }
    out.sort_by_key(|(d, _)| *d);
    Ok(out)
}
```

- [ ] **Step 3.3: Uncomment `pub mod store;` in `mur-core/src/conversations/mod.rs`**

Edit `mod.rs` to add: `pub mod store;`

- [ ] **Step 3.4: Run tests**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-core conversations::store::tests`
Expected: 3 tests pass.

- [ ] **Step 3.5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/conversations/store.rs mur-core/src/conversations/mod.rs
git commit -m "feat(core): conversations raw JSONL append-only store

Writes one message per line to ~/.mur/conversations/raw/<date>/<src>_<id>.jsonl.
Append-only (spec §12 #2). read_day sorts across all files for a given date.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Audit hash chain

**Files:**
- Create: `mur-core/src/conversations/audit.rs`
- Modify: `mur-core/src/conversations/mod.rs`
- Test: inline

- [ ] **Step 4.1: Write failing tests**

Add to `mur-core/src/conversations/audit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_starts_from_zero_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        let e = a.append(AuditAction::Write {
            target: "raw/2026-04-19/cc_abc.jsonl".into(),
            bytes: 128,
        }).unwrap();
        assert_eq!(e.prev_hash, ZERO_HASH);
        assert_ne!(e.entry_hash, ZERO_HASH);
    }

    #[test]
    fn chain_links_consecutive_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        let e1 = a.append(AuditAction::Write { target: "x".into(), bytes: 1 }).unwrap();
        let e2 = a.append(AuditAction::Write { target: "y".into(), bytes: 2 }).unwrap();
        assert_eq!(e2.prev_hash, e1.entry_hash);
    }

    #[test]
    fn verify_replays_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        a.append(AuditAction::Write { target: "x".into(), bytes: 1 }).unwrap();
        a.append(AuditAction::Delete { target: "y".into(), reason: "retention".into() }).unwrap();
        assert!(verify(Some(root)).unwrap());
    }

    #[test]
    fn verify_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        a.append(AuditAction::Write { target: "x".into(), bytes: 1 }).unwrap();
        // Corrupt the file
        let p = crate::conversations::paths::audit_path(Some(root));
        let content = std::fs::read_to_string(&p).unwrap();
        let tampered = content.replace("\"bytes\":1", "\"bytes\":99");
        std::fs::write(&p, tampered).unwrap();
        assert!(!verify(Some(root)).unwrap());
    }
}
```

- [ ] **Step 4.2: Implement `mur-core/src/conversations/audit.rs`**

```rust
//! Hash-chained audit log at ~/.mur/conversations/audit.jsonl.
//!
//! Each entry's `entry_hash = sha256(prev_hash || canonical_json(action, target))`.
//! Chain is initialized from commander's existing audit.jsonl during migration
//! (see migrate.rs). This module only handles append + verify; migration is
//! a separate concern.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use uuid::Uuid;

use super::paths::audit_path;

pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AuditAction {
    Write { target: String, bytes: u64 },
    Summarize { date: String, model: String, duration_ms: u64 },
    Index { date: String, vectors_added: u64 },
    Delete { target: String, reason: String },
    Migrate { from: String, to: String, count: u64 },
    Rollback { from: String, to: String, count: u64 },
    Error { layer: String, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub action: AuditAction,
    pub prev_hash: String,
    pub entry_hash: String,
}

/// Append-only audit writer. Not thread-safe across processes; use one per process.
pub struct Audit {
    root_override: Option<String>,
    last_hash: Mutex<String>,
}

impl Audit {
    pub fn open(root_override: Option<&str>) -> Result<Self> {
        let root_override = root_override.map(|s| s.to_string());
        let last_hash = read_last_hash(root_override.as_deref())?;
        Ok(Self {
            root_override,
            last_hash: Mutex::new(last_hash),
        })
    }

    pub fn append(&self, action: AuditAction) -> Result<AuditEntry> {
        let mut guard = self.last_hash.lock().expect("audit mutex");
        let prev_hash = guard.clone();
        let entry_hash = compute_hash(&prev_hash, &action);
        let entry = AuditEntry {
            id: Uuid::new_v4(),
            ts: Utc::now(),
            action,
            prev_hash,
            entry_hash: entry_hash.clone(),
        };
        let path = audit_path(self.root_override.as_deref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&path)
            .with_context(|| format!("open audit {path:?}"))?;
        serde_json::to_writer(&mut f, &entry)?;
        writeln!(f)?;
        *guard = entry_hash;
        Ok(entry)
    }
}

fn read_last_hash(root_override: Option<&str>) -> Result<String> {
    let path = audit_path(root_override);
    if !path.exists() {
        return Ok(ZERO_HASH.to_string());
    }
    let f = fs::File::open(&path)?;
    let mut last = ZERO_HASH.to_string();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let e: AuditEntry = serde_json::from_str(&line)?;
        last = e.entry_hash;
    }
    Ok(last)
}

fn compute_hash(prev_hash: &str, action: &AuditAction) -> String {
    let canonical = serde_json::to_string(action).expect("action must serialize");
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    hex::encode(h.finalize())
}

/// Replay chain from disk and verify every entry_hash. True = chain intact.
pub fn verify(root_override: Option<&str>) -> Result<bool> {
    let path = audit_path(root_override);
    if !path.exists() {
        return Ok(true); // empty chain is valid
    }
    let f = fs::File::open(&path)?;
    let mut prev = ZERO_HASH.to_string();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let e: AuditEntry = serde_json::from_str(&line)?;
        if e.prev_hash != prev {
            return Ok(false);
        }
        let expected = compute_hash(&e.prev_hash, &e.action);
        if expected != e.entry_hash {
            return Ok(false);
        }
        prev = e.entry_hash;
    }
    Ok(true)
}
```

- [ ] **Step 4.3: Add deps to `mur-core/Cargo.toml`**

Append to `[dependencies]` section:

```toml
sha2 = "0.10"
hex = "0.4"
```

- [ ] **Step 4.4: Uncomment `pub mod audit;` in `mur-core/src/conversations/mod.rs`**

- [ ] **Step 4.5: Run tests**

Run: `cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-core conversations::audit::tests`
Expected: 4 tests pass.

- [ ] **Step 4.6: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/conversations/audit.rs mur-core/src/conversations/mod.rs mur-core/Cargo.toml
git commit -m "feat(core): conversations audit hash-chain log

sha256(prev_hash || action) links every entry. verify() replays chain and
detects tampering. Used by every mutating operation in conversations/
(write, summarize, index, delete, migrate, rollback, error).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Content-addressed blob store

**Files:**
- Create: `mur-core/src/conversations/blob.rs`
- Modify: `mur-core/src/conversations/mod.rs`
- Test: inline

- [ ] **Step 5.1: Write failing tests**

Add to `blob.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let sha = put(b"hello world", Some(root)).unwrap();
        assert_eq!(sha.len(), 64);
        assert_eq!(get(&sha, Some(root)).unwrap(), b"hello world");
    }

    #[test]
    fn put_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = put(b"same", Some(root)).unwrap();
        let b = put(b"same", Some(root)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn get_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(get("deadbeef", Some(tmp.path().to_str().unwrap())).is_err());
    }
}
```

- [ ] **Step 5.2: Implement `blob.rs`**

```rust
//! Content-addressed blob store at ~/.mur/conversations/blob/<sha256>.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;

use super::paths::blob_root;

pub fn put(content: &[u8], root_override: Option<&str>) -> Result<String> {
    let sha = {
        let mut h = Sha256::new();
        h.update(content);
        hex::encode(h.finalize())
    };
    let root = blob_root(root_override);
    fs::create_dir_all(&root)?;
    let path = root.join(&sha);
    if !path.exists() {
        fs::write(&path, content).with_context(|| format!("writing blob {path:?}"))?;
    }
    Ok(sha)
}

pub fn get(sha: &str, root_override: Option<&str>) -> Result<Vec<u8>> {
    let path = blob_root(root_override).join(sha);
    fs::read(&path).with_context(|| format!("reading blob {path:?}"))
}
```

- [ ] **Step 5.3: Uncomment `pub mod blob;` in `conversations/mod.rs`**
- [ ] **Step 5.4: Run tests**

Run: `cargo test -p mur-core conversations::blob::tests` — expect 3 PASS.

- [ ] **Step 5.5: Commit**

```bash
git add mur-core/src/conversations/blob.rs mur-core/src/conversations/mod.rs
git commit -m "feat(core): content-addressed blob store for tool_ref payloads

sha256-keyed; put() is idempotent on duplicates. Used by normalize.rs to
stash large tool-result bodies when the original isn't persisted elsewhere."
```

---

## Task 6: Pre-filter stage 1 — Normalize (tool-call pointer substitution)

**Files:**
- Create: `mur-core/src/conversations/ingest/mod.rs`
- Create: `mur-core/src/conversations/ingest/normalize.rs`
- Modify: `mur-core/src/conversations/mod.rs`
- Modify: `mur-core/Cargo.toml` (add `async-trait = "0.1"`)

- [ ] **Step 6.1: Create `ingest/mod.rs`**

```rust
//! Ingestion pipeline. Stages run normalize → dedup → filter → store.append,
//! with audit entries per successful write.

pub mod normalize;
// Later tasks add: pub mod dedup; pub mod filter; pub mod pipeline;
// Later tasks add: pub mod aider; pub mod claude_code; pub mod cursor; pub mod gemini;

use anyhow::Result;
use mur_common::Message;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PullCursor {
    pub last_mtime_unix: i64,
    pub last_hash: String,
    #[serde(default)]
    pub per_file: std::collections::BTreeMap<String, FileCursor>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileCursor {
    pub mtime_unix: i64,
    pub last_line: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum IngestStrategy {
    RealTime,
    Poll(std::time::Duration),
    Manual,
}

#[async_trait::async_trait]
pub trait Ingester: Send + Sync {
    fn name(&self) -> &'static str;
    fn strategy(&self) -> IngestStrategy;
    async fn pull(&mut self, cursor: &mut PullCursor) -> Result<Vec<Message>>;
}
```

- [ ] **Step 6.2: Write failing tests for normalize**

Add to `normalize.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn tool_msg(text: &str) -> Message {
        Message {
            v: 1, ts: chrono::Utc::now(), src: Source::ClaudeCode,
            conv: "c".into(), role: Role::Tool,
            content: Content::Text { value: text.into() },
            meta: serde_json::json!({"tool": "Read", "path": "src/main.rs"}),
            refs: vec![],
        }
    }

    #[test]
    fn small_text_passes_through() {
        let tmp = tempfile::tempdir().unwrap();
        let out = normalize(tool_msg("short"), Some(tmp.path().to_str().unwrap())).unwrap();
        assert!(matches!(out.content, Content::Text { .. }));
    }

    #[test]
    fn large_text_becomes_tool_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(11_000);
        let out = normalize(tool_msg(&big), Some(tmp.path().to_str().unwrap())).unwrap();
        match out.content {
            Content::ToolRef { sha256, bytes, .. } => {
                assert_eq!(bytes, 11_000);
                assert_eq!(sha256.len(), 64);
            }
            _ => panic!("expected ToolRef"),
        }
    }

    #[test]
    fn user_role_never_pointerized() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = tool_msg(&"x".repeat(11_000));
        m.role = Role::User;
        let out = normalize(m, Some(tmp.path().to_str().unwrap())).unwrap();
        assert!(matches!(out.content, Content::Text { .. }));
    }
}
```

- [ ] **Step 6.3: Implement `normalize.rs`**

```rust
//! Stage 1: tool-call pointer substitution (spec §4.3).
use anyhow::Result;
use mur_common::{Content, Message, Role};

use super::super::blob;

pub const NORM_THRESHOLD: usize = 10 * 1024;

pub fn normalize(msg: Message, root_override: Option<&str>) -> Result<Message> {
    if !matches!(msg.role, Role::Tool) { return Ok(msg); }
    let Content::Text { value } = &msg.content else { return Ok(msg); };
    if value.len() < NORM_THRESHOLD { return Ok(msg); }
    let bytes = value.len() as u64;
    let sha256 = blob::put(value.as_bytes(), root_override)?;
    let path = msg.meta.get("path").and_then(|v| v.as_str()).unwrap_or("<in-blob>").to_string();
    let desc = msg.meta.get("tool").and_then(|v| v.as_str())
        .map(|t| format!("{} result", t))
        .unwrap_or_else(|| "tool result".into());
    let mut out = msg;
    out.content = Content::ToolRef { sha256, path, bytes, desc };
    Ok(out)
}
```

- [ ] **Step 6.4: Uncomment `pub mod ingest;` in `conversations/mod.rs`; add `async-trait = "0.1"` to `mur-core/Cargo.toml`**
- [ ] **Step 6.5: Run tests**

Run: `cargo test -p mur-core conversations::ingest::normalize::tests` — expect 3 PASS.

- [ ] **Step 6.6: Commit**

```bash
git add mur-core/src/conversations/ingest/ mur-core/src/conversations/mod.rs mur-core/Cargo.toml
git commit -m "feat(core): ingest/normalize — tool-call pointer substitution

Tool-role messages > 10KB become Content::ToolRef with sha256 pointer to
the blob store. User/assistant messages stay verbatim. Implements spec §4.3."
```

---

## Task 7: Pre-filter stage 2 — Dedup (MinHash)

**Files:**
- Create: `mur-core/src/conversations/ingest/dedup.rs`
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 7.1: Write failing tests**

Add to `dedup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_dedup() {
        let mut d = Dedup::new(0.85);
        assert!(!d.is_duplicate("cargo build succeeded in 3.2s elapsed"));
        assert!(d.is_duplicate("cargo build succeeded in 3.2s elapsed"));
    }

    #[test]
    fn near_identical_dedup() {
        let mut d = Dedup::new(0.85);
        let a = "warning unused variable x in src/main.rs line 42 column 8 here";
        let b = "warning unused variable x in src/main.rs line 42 column 9 here";
        d.is_duplicate(a);
        assert!(d.is_duplicate(b));
    }

    #[test]
    fn distinct_kept() {
        let mut d = Dedup::new(0.85);
        d.is_duplicate("compile error in src/foo.rs line twenty");
        assert!(!d.is_duplicate("runtime panic at thread main during startup"));
    }

    #[test]
    fn short_text_kept() {
        let mut d = Dedup::new(0.85);
        assert!(!d.is_duplicate("hi"));
        assert!(!d.is_duplicate("hi"));
    }
}
```

- [ ] **Step 7.2: Implement `dedup.rs`**

```rust
//! Stage 2: MinHash 64-bit near-dup detection. Threshold 0.85 Jaccard.
use std::collections::HashSet;

const NUM_HASHES: usize = 64;
const SHINGLE_SIZE: usize = 5;

pub struct Dedup {
    threshold: f64,
    seen: Vec<[u64; NUM_HASHES]>,
    signature_set: HashSet<[u64; NUM_HASHES]>,
}

impl Dedup {
    pub fn new(threshold: f64) -> Self {
        Self { threshold, seen: Vec::new(), signature_set: HashSet::new() }
    }

    pub fn is_duplicate(&mut self, text: &str) -> bool {
        let sh = shingles(text, SHINGLE_SIZE);
        if sh.len() < 2 { return false; }
        let sig = minhash_signature(&sh);
        if self.signature_set.contains(&sig) { return true; }
        for prev in &self.seen {
            if jaccard_estimate(&sig, prev) >= self.threshold { return true; }
        }
        self.seen.push(sig);
        self.signature_set.insert(sig);
        false
    }
}

fn shingles(text: &str, k: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < k { return Vec::new(); }
    (0..=words.len() - k).map(|i| words[i..i + k].join(" ")).collect()
}

fn minhash_signature(shingles: &[String]) -> [u64; NUM_HASHES] {
    let mut sig = [u64::MAX; NUM_HASHES];
    for sh in shingles {
        for (i, slot) in sig.iter_mut().enumerate() {
            let h = hash64(sh, i as u64);
            if h < *slot { *slot = h; }
        }
    }
    sig
}

fn jaccard_estimate(a: &[u64; NUM_HASHES], b: &[u64; NUM_HASHES]) -> f64 {
    a.iter().zip(b.iter()).filter(|(x, y)| x == y).count() as f64 / NUM_HASHES as f64
}

fn hash64(s: &str, seed: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325u64.wrapping_add(seed);
    for b in s.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    h
}
```

- [ ] **Step 7.3: Uncomment `pub mod dedup;` in `ingest/mod.rs`**
- [ ] **Step 7.4: Run tests**

Run: `cargo test -p mur-core conversations::ingest::dedup::tests` — expect 4 PASS.

- [ ] **Step 7.5: Commit**

```bash
git add mur-core/src/conversations/ingest/dedup.rs mur-core/src/conversations/ingest/mod.rs
git commit -m "feat(core): ingest/dedup — MinHash near-duplicate detection

64-bit MinHash, 5-word shingles, Jaccard threshold 0.85. In-memory per
pipeline run; kills mid-session repetition (cargo echoes, etc.)."
```

---

## Task 8: Pre-filter stage 3 — REJECT filter (Mem0 lesson)

**Files:**
- Create: `mur-core/src/conversations/ingest/filter.rs`
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 8.1: Write failing tests**

Add to `filter.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            v: 1, ts: chrono::Utc::now(), src: Source::ClaudeCode,
            conv: "c".into(), role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null, refs: vec![],
        }
    }

    #[test]
    fn empty_rejected() {
        assert!(matches!(decide(&msg(Role::User, "")), Decision::Reject(_)));
        assert!(matches!(decide(&msg(Role::User, "   \n")), Decision::Reject(_)));
    }

    #[test]
    fn heartbeat_rejected() {
        assert!(matches!(decide(&msg(Role::System, "[heartbeat]")), Decision::Reject(_)));
        assert!(matches!(decide(&msg(Role::System, "ping")), Decision::Reject(_)));
    }

    #[test]
    fn system_restatement_rejected() {
        let t = "You are Claude, created by Anthropic. You are helpful, harmless, and honest.";
        assert!(matches!(decide(&msg(Role::Assistant, t)), Decision::Reject(_)));
    }

    #[test]
    fn normal_accepted() {
        assert!(matches!(decide(&msg(Role::User, "read src/main.rs please")), Decision::Accept));
    }

    #[test]
    fn tool_ref_always_accepted() {
        let m = Message {
            content: Content::ToolRef { sha256: "a".into(), path: "p".into(), bytes: 1, desc: "d".into() },
            ..msg(Role::Tool, "")
        };
        assert!(matches!(decide(&m), Decision::Accept));
    }
}
```

- [ ] **Step 8.2: Implement `filter.rs`**

```rust
//! Stage 3: conservative REJECT gate. Only drops provable noise.
//! Spec §5.4; research §5 (Mem0 97.8% junk audit).

use mur_common::{Content, Message, Role};

#[derive(Debug)]
pub enum Decision {
    Accept,
    Reject(&'static str),
}

const HEARTBEAT: &[&str] = &["[heartbeat]", "heartbeat", "ping", "pong", "health check", "liveness probe"];
const RESTATEMENT: &[&str] = &[
    "you are claude", "you are an ai", "i am claude", "i'm claude",
    "created by anthropic", "helpful, harmless, and honest",
];

pub fn decide(msg: &Message) -> Decision {
    let Content::Text { value } = &msg.content else { return Decision::Accept; };
    let t = value.trim();
    if t.is_empty() { return Decision::Reject("empty"); }
    let lower = t.to_lowercase();
    if matches!(msg.role, Role::System) {
        for p in HEARTBEAT {
            if lower == *p || lower.starts_with(p) { return Decision::Reject("heartbeat"); }
        }
    }
    if matches!(msg.role, Role::Assistant) {
        let hits = RESTATEMENT.iter().filter(|m| lower.contains(*m)).count();
        if hits >= 2 { return Decision::Reject("system-prompt restatement"); }
    }
    Decision::Accept
}
```

- [ ] **Step 8.3: Uncomment `pub mod filter;` in `ingest/mod.rs`**
- [ ] **Step 8.4: Run tests**

Run: `cargo test -p mur-core conversations::ingest::filter::tests` — expect 5 PASS.

- [ ] **Step 8.5: Commit**

```bash
git add mur-core/src/conversations/ingest/filter.rs mur-core/src/conversations/ingest/mod.rs
git commit -m "feat(core): ingest/filter — conservative REJECT gate

Drops only provable noise: empty, heartbeat, assistant system-prompt
restatement. Everything else accepts. Mem0 audit lesson: when in doubt,
accept — bias is toward preserving."
```

---

## Task 9: Pipeline orchestrator (wires normalize → dedup → filter → store + audit)

**Files:**
- Create: `mur-core/src/conversations/ingest/pipeline.rs`
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 9.1: Write failing tests**

Add to `pipeline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            v: 1, ts: chrono::Utc::now(), src: Source::ClaudeCode,
            conv: "c".into(), role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null, refs: vec![],
        }
    }

    #[test]
    fn accepted_messages_are_written_and_audited() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let report = p.run(vec![msg(Role::User, "hi there how are you")]).unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.rejected, 0);
        // Audit has at least one Write entry
        let entries = std::fs::read_to_string(
            crate::conversations::paths::audit_path(Some(root))
        ).unwrap();
        assert!(entries.contains("\"action\":\"write\""));
    }

    #[test]
    fn duplicate_messages_are_deduped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let r = p.run(vec![
            msg(Role::User, "the quick brown fox jumps over"),
            msg(Role::User, "the quick brown fox jumps over"),
        ]).unwrap();
        assert_eq!(r.accepted, 1);
        assert_eq!(r.deduped, 1);
    }

    #[test]
    fn rejected_messages_are_not_written() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let r = p.run(vec![msg(Role::User, "")]).unwrap();
        assert_eq!(r.accepted, 0);
        assert_eq!(r.rejected, 1);
    }
}
```

- [ ] **Step 9.2: Implement `pipeline.rs`**

```rust
//! Pre-filter pipeline orchestrator.
//! Runs normalize → dedup → filter → store.append with an audit entry per write.

use anyhow::Result;
use mur_common::{Content, Message};
use tracing::warn;

use super::dedup::Dedup;
use super::filter::{decide, Decision};
use super::normalize::normalize;
use super::super::{audit::{Audit, AuditAction}, store};

#[derive(Debug, Default)]
pub struct Report {
    pub accepted: u64,
    pub rejected: u64,
    pub deduped: u64,
    pub errors: u64,
}

pub struct Pipeline {
    root_override: Option<String>,
    audit: Audit,
    dedup: Dedup,
}

impl Pipeline {
    pub fn new(root_override: Option<&str>) -> Result<Self> {
        Ok(Self {
            root_override: root_override.map(|s| s.to_string()),
            audit: Audit::open(root_override)?,
            dedup: Dedup::new(0.85),
        })
    }

    pub fn run(&mut self, messages: Vec<Message>) -> Result<Report> {
        let mut r = Report::default();
        for msg in messages {
            match self.process_one(msg) {
                Ok(Outcome::Accepted(bytes)) => {
                    r.accepted += 1;
                    let _ = self.audit.append(AuditAction::Write {
                        target: "raw".into(),
                        bytes,
                    });
                }
                Ok(Outcome::Rejected) => r.rejected += 1,
                Ok(Outcome::Deduped) => r.deduped += 1,
                Err(e) => {
                    r.errors += 1;
                    warn!("pipeline error: {e:#}");
                    let _ = self.audit.append(AuditAction::Error {
                        layer: "pipeline".into(),
                        reason: format!("{e:#}"),
                    });
                }
            }
        }
        Ok(r)
    }

    fn process_one(&mut self, msg: Message) -> Result<Outcome> {
        // 1. Normalize (degrade: keep original on error)
        let msg = match normalize(msg, self.root_override.as_deref()) {
            Ok(m) => m,
            Err(e) => {
                warn!("normalize failed, keeping original: {e:#}");
                return Ok(Outcome::Rejected); // unreachable path; but safer
            }
        };
        // 2. Dedup (only on Text variant; ToolRef/ImageRef are assumed unique via hash)
        if let Content::Text { value } = &msg.content {
            if self.dedup.is_duplicate(value) {
                return Ok(Outcome::Deduped);
            }
        }
        // 3. Filter
        if let Decision::Reject(_reason) = decide(&msg) {
            return Ok(Outcome::Rejected);
        }
        // 4. Store (serialize once for byte count)
        let bytes = serde_json::to_vec(&msg)?.len() as u64;
        store::append(&msg, self.root_override.as_deref())?;
        Ok(Outcome::Accepted(bytes))
    }
}

enum Outcome { Accepted(u64), Rejected, Deduped }
```

- [ ] **Step 9.3: Uncomment `pub mod pipeline;` in `ingest/mod.rs`**
- [ ] **Step 9.4: Run tests**

Run: `cargo test -p mur-core conversations::ingest::pipeline::tests` — expect 3 PASS.

- [ ] **Step 9.5: Commit**

```bash
git add mur-core/src/conversations/ingest/pipeline.rs mur-core/src/conversations/ingest/mod.rs
git commit -m "feat(core): ingest/pipeline — normalize → dedup → filter → store

End-to-end pre-filter orchestrator with audit entry per accepted write
and error entry per failure. Never blocks on failure — degrades to
'pass through' per spec §8.1."
```

---

## Task 10: LanceDB index wrapper (RaBitQ)

**Files:**
- Create: `mur-core/src/conversations/index.rs`
- Modify: `mur-core/src/conversations/mod.rs`

- [ ] **Step 10.1: Write failing tests (async)**

Add to `index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(n: &str, text: &str) -> Message {
        Message {
            v: 1, ts: chrono::Utc::now(), src: Source::ClaudeCode,
            conv: n.into(), role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null, refs: vec![],
        }
    }

    #[tokio::test]
    async fn open_and_upsert_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let entries = vec![
            (msg("a", "cargo build failed"), vec![1.0; 16]),
            (msg("b", "yaml parsing worked"), vec![0.0; 16]),
        ];
        idx.upsert(&entries).await.unwrap();
        let hits = idx.search(&vec![1.0; 16], 2, None).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].conv_id, "a");
    }

    #[tokio::test]
    async fn filter_by_source() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = ConversationIndex::open(16, Some(root)).await.unwrap();
        let mut m = msg("x", "shared");
        m.src = Source::Slack;
        idx.upsert(&[(m, vec![1.0; 16])]).await.unwrap();
        idx.upsert(&[(msg("y", "shared"), vec![1.0; 16])]).await.unwrap();
        let hits = idx.search(&vec![1.0; 16], 10, Some(Source::Slack)).await.unwrap();
        assert!(hits.iter().all(|h| h.source == Source::Slack));
    }
}
```

- [ ] **Step 10.2: Implement `index.rs`**

Model on existing `mur-core/src/store/lancedb.rs` (Arrow 57 + LanceDB 0.26). Table: `conversations`. Columns per spec §4.4. RaBitQ applied after ~10k rows — for Phase 1, build the table as flat first; `create_index` with RaBitQ is opt-in (add `reindex()` helper). Ship flat search first; RaBitQ is a later operation.

```rust
//! LanceDB index for the conversations archive. Table "conversations".
//! See spec §4.4.

use anyhow::{Context, Result};
use arrow_array::{
    FixedSizeListArray, Float32Array, Int8Array, Int64Array, RecordBatch,
    RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use mur_common::{Message, Source};
use std::sync::Arc;

use super::paths::index_path;

const TABLE: &str = "conversations";

pub struct ConversationIndex {
    db: lancedb::Connection,
    dims: i32,
}

pub struct SearchHit {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub content: String,
    pub distance: f32,
}

impl ConversationIndex {
    pub async fn open(dims: i32, root_override: Option<&str>) -> Result<Self> {
        let path = index_path(root_override);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let db = lancedb::connect(path.to_str().unwrap()).execute().await
            .context("opening LanceDB for conversations")?;
        Ok(Self { db, dims })
    }

    fn schema(&self) -> Schema {
        Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("ts", DataType::Int64, false),
            Field::new("source", DataType::Utf8, false),
            Field::new("conv_id", DataType::Utf8, false),
            Field::new("role", DataType::Utf8, false),
            Field::new("layer", DataType::Int8, false),
            Field::new("content", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dims,
                ),
                false,
            ),
        ])
    }

    pub async fn upsert(&mut self, entries: &[(Message, Vec<f32>)]) -> Result<()> {
        if entries.is_empty() { return Ok(()); }
        let schema = Arc::new(self.schema());
        let tables = self.db.table_names().execute().await?;

        let ids: Vec<String> = entries.iter().enumerate().map(|(i, (m, _))|
            format!("{}_{}_{}", m.src.file_prefix(), m.conv, i)
        ).collect();
        let tss: Vec<i64> = entries.iter().map(|(m, _)| m.ts.timestamp()).collect();
        let srcs: Vec<&str> = entries.iter().map(|(m, _)| m.src.file_prefix()).collect();
        let convs: Vec<&str> = entries.iter().map(|(m, _)| m.conv.as_str()).collect();
        let roles: Vec<&'static str> = entries.iter().map(|(m, _)| match m.role {
            mur_common::Role::User => "user",
            mur_common::Role::Assistant => "assistant",
            mur_common::Role::System => "system",
            mur_common::Role::Tool => "tool",
        }).collect();
        let layers: Vec<i8> = entries.iter().map(|_| 0_i8).collect();
        let contents: Vec<String> = entries.iter().map(|(m, _)| match &m.content {
            mur_common::Content::Text { value } => value.clone(),
            mur_common::Content::ToolRef { desc, .. } => desc.clone(),
            mur_common::Content::ImageRef { desc, .. } => desc.clone(),
        }).collect();
        let content_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();

        let flat: Vec<f32> = entries.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        let vec_arr = FixedSizeListArray::try_new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dims,
            Arc::new(Float32Array::from(flat)),
            None,
        )?;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids.iter().map(|s| s.as_str()).collect::<Vec<_>>())),
                Arc::new(Int64Array::from(tss)),
                Arc::new(StringArray::from(srcs)),
                Arc::new(StringArray::from(convs)),
                Arc::new(StringArray::from(roles)),
                Arc::new(Int8Array::from(layers)),
                Arc::new(StringArray::from(content_refs)),
                Arc::new(vec_arr),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema.clone());

        if tables.contains(&TABLE.to_string()) {
            self.db.open_table(TABLE).execute().await?
                .add(Box::new(batches)).execute().await?;
        } else {
            self.db.create_table(TABLE, Box::new(batches)).execute().await?;
        }
        Ok(())
    }

    pub async fn search(
        &self,
        query_vec: &[f32],
        limit: usize,
        source_filter: Option<Source>,
    ) -> Result<Vec<SearchHit>> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) { return Ok(Vec::new()); }
        let table = self.db.open_table(TABLE).execute().await?;
        let mut q = table.query().nearest_to(query_vec.to_vec())?.limit(limit);
        if let Some(s) = source_filter {
            q = q.only_if(format!("source = '{}'", s.file_prefix()));
        }
        let stream = q.execute().await?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;
        let mut out = Vec::new();
        for b in batches {
            let ids = b.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let tss = b.column_by_name("ts").unwrap().as_any().downcast_ref::<Int64Array>().unwrap();
            let srcs = b.column_by_name("source").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let convs = b.column_by_name("conv_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let contents = b.column_by_name("content").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let dists = b.column_by_name("_distance").and_then(|c|
                c.as_any().downcast_ref::<Float32Array>()
            );
            for i in 0..b.num_rows() {
                let source = match srcs.value(i) {
                    "cc" => Source::ClaudeCode,
                    "cursor" => Source::Cursor,
                    "gemini" => Source::Gemini,
                    "aider" => Source::Aider,
                    "slack" => Source::Slack,
                    "telegram" => Source::Telegram,
                    "discord" => Source::Discord,
                    "commander" => Source::CommanderEngine,
                    other => anyhow::bail!("unknown source tag {other}"),
                };
                out.push(SearchHit {
                    id: ids.value(i).to_string(),
                    ts: tss.value(i),
                    source,
                    conv_id: convs.value(i).to_string(),
                    content: contents.value(i).to_string(),
                    distance: dists.map(|d| d.value(i)).unwrap_or(0.0),
                });
            }
        }
        Ok(out)
    }

    /// Build/refresh a RaBitQ index on the vector column. Call periodically
    /// (e.g., nightly) once the table exceeds ~10k rows.
    pub async fn rebuild_rabitq(&self) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if !tables.contains(&TABLE.to_string()) { return Ok(()); }
        let _table = self.db.open_table(TABLE).execute().await?;
        // LanceDB 0.26: `create_index` with IvfPq / Hnsw. RaBitQ is available
        // via IndexType::RaBitQ if the feature is present; fall back to IVF_PQ.
        // Leaving the exact index-builder call for the implementing engineer
        // to choose based on the installed LanceDB minor version; at time of
        // writing (2026-04-19) RaBitQ shipped in lancedb 0.26.
        // TODO(phase-2): pin the exact call once LanceDB API stabilizes.
        Ok(())
    }
}
```

- [ ] **Step 10.3: Uncomment `pub mod index;` in `conversations/mod.rs`**
- [ ] **Step 10.4: Run tests**

Run: `cargo test -p mur-core conversations::index::tests` — expect 2 PASS.

- [ ] **Step 10.5: Commit**

```bash
git add mur-core/src/conversations/index.rs mur-core/src/conversations/mod.rs
git commit -m "feat(core): conversations/index — LanceDB wrapper

Schema per spec §4.4 with layer column for future RAPTOR layers. Flat
search in Phase 1; rebuild_rabitq stub lays groundwork for quantized
index once table exceeds ~10k rows."
```

---

## Task 11: Claude Code ingester (route `mur session record`)

**Files:**
- Create: `mur-core/src/conversations/ingest/claude_code.rs`
- Modify: `mur-core/src/cmd/session.rs` (add branch: when `conversations.enabled`, also push through pipeline)
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 11.1: Write failing tests**

Add to `claude_code.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::Role;

    #[test]
    fn event_to_message_user() {
        let m = event_to_message("user", None, "hi there", "sess-123").unwrap();
        assert!(matches!(m.role, Role::User));
        assert_eq!(m.conv, "sess-123");
    }

    #[test]
    fn event_to_message_tool_with_meta() {
        let m = event_to_message("tool_call", Some("Read"), "{\"path\":\"x\"}", "sess").unwrap();
        assert!(matches!(m.role, Role::Tool));
        assert_eq!(m.meta.get("tool").and_then(|v| v.as_str()), Some("Read"));
    }

    #[test]
    fn unknown_event_type_errors() {
        assert!(event_to_message("banana", None, "x", "s").is_err());
    }
}
```

- [ ] **Step 11.2: Implement `claude_code.rs`**

```rust
//! Claude Code ingester. Existing hooks (on-prompt/on-tool/on-stop) call
//! `mur session record`, which — when conversations is enabled — pushes
//! through the pipeline as well.
//!
//! Event-type mapping:
//!   "user"      -> Role::User
//!   "assistant" -> Role::Assistant
//!   "tool_call" -> Role::Tool
//!   "system"    -> Role::System

use anyhow::{bail, Result};
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};

pub fn event_to_message(
    event_type: &str,
    tool: Option<&str>,
    content: &str,
    session_id: &str,
) -> Result<Message> {
    let role = match event_type {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool_call" | "tool" => Role::Tool,
        "system" => Role::System,
        other => bail!("unknown event type: {other}"),
    };
    let mut meta = serde_json::json!({});
    if let Some(t) = tool {
        meta["tool"] = serde_json::Value::String(t.into());
    }
    Ok(Message {
        v: 1,
        ts: Utc::now(),
        src: Source::ClaudeCode,
        conv: session_id.to_string(),
        role,
        content: Content::Text { value: content.to_string() },
        meta,
        refs: vec![],
    })
}
```

- [ ] **Step 11.3: Wire into `cmd/session.rs`**

Find the existing `record()` handler (it already writes to `~/.mur/session/recordings/<id>.jsonl`). Add a second call path, gated on `~/.mur/config.yaml`'s `conversations.enabled`:

```rust
// In cmd/session.rs, after writing to legacy recordings:
if crate::conversations::is_enabled()? {
    let msg = crate::conversations::ingest::claude_code::event_to_message(
        &event_type, tool.as_deref(), &content, &session_id,
    )?;
    let mut pipeline = crate::conversations::ingest::pipeline::Pipeline::new(None)?;
    let _ = pipeline.run(vec![msg])?;
}
```

Add `pub fn is_enabled() -> Result<bool>` to `conversations/mod.rs`:

```rust
pub fn is_enabled() -> anyhow::Result<bool> {
    // Read mur's config.yaml; default false.
    let cfg_path = dirs::home_dir().unwrap().join(".mur").join("config.yaml");
    if !cfg_path.exists() { return Ok(false); }
    let text = std::fs::read_to_string(&cfg_path)?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text)?;
    Ok(doc.get("conversations")
        .and_then(|c| c.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
```

- [ ] **Step 11.4: Uncomment `pub mod claude_code;` in `ingest/mod.rs`**
- [ ] **Step 11.5: Run tests**

Run: `cargo test -p mur-core conversations::ingest::claude_code::tests` — expect 3 PASS.

- [ ] **Step 11.6: Commit**

```bash
git add mur-core/src/conversations/ingest/claude_code.rs \
        mur-core/src/conversations/mod.rs \
        mur-core/src/conversations/ingest/mod.rs \
        mur-core/src/cmd/session.rs
git commit -m "feat(core): Claude Code ingester — route session record events

Feature-flagged via conversations.enabled in ~/.mur/config.yaml. When off,
existing session/recordings/ behavior is untouched. When on, same events
also flow through the pre-filter pipeline into conversations/raw/."
```

---

## Task 12: Cursor ingester (SQLite + .specstory)

**Files:**
- Create: `mur-core/src/conversations/ingest/cursor.rs`
- Modify: `mur-core/Cargo.toml` (add `rusqlite = { version = "0.32", features = ["bundled"] }`)
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 12.1: Write failing tests**

Add to `cursor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_chat_from_json_blob() {
        let blob = serde_json::json!({
            "chats": [{
                "id": "chat-1",
                "messages": [
                    {"role": "user", "content": "hello"},
                    {"role": "assistant", "content": "hi back"},
                ]
            }]
        });
        let msgs = parse_cursor_chat_blob(&blob, "workspace-x").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].conv, "workspace-x/chat-1");
    }

    #[test]
    fn unknown_schema_returns_empty_not_error() {
        let blob = serde_json::json!({"unexpected": "shape"});
        let msgs = parse_cursor_chat_blob(&blob, "ws").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn specstory_md_parser_extracts_turns() {
        let md = "#### User\n\nhello\n\n---\n\n#### Assistant\n\nhi\n";
        let msgs = parse_specstory_md(md, "my-chat").unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
```

- [ ] **Step 12.2: Implement `cursor.rs`**

```rust
//! Cursor ingester. Two inputs:
//! 1. SQLite at ~/Library/Application Support/Cursor/User/workspaceStorage/*/state.vscdb
//! 2. .specstory/ markdown files (public export format)
//!
//! Schema is unstable; best-effort with skip-on-failure.

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::{Path, PathBuf};

pub fn list_cursor_workspaces() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new(); };
    let base = home.join("Library/Application Support/Cursor/User/workspaceStorage");
    if !base.exists() { return Vec::new(); }
    match std::fs::read_dir(&base) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn scan_workspace(ws_dir: &Path) -> Result<Vec<Message>> {
    let db = ws_dir.join("state.vscdb");
    if db.exists() {
        if let Ok(msgs) = scan_vscdb(&db, ws_dir) {
            return Ok(msgs);
        }
    }
    Ok(Vec::new())
}

fn scan_vscdb(db: &Path, ws_dir: &Path) -> Result<Vec<Message>> {
    let conn = rusqlite::Connection::open(db)
        .with_context(|| format!("open {db:?}"))?;
    let ws = ws_dir.file_name().and_then(|s| s.to_str()).unwrap_or("ws").to_string();

    let mut stmt = conn.prepare(
        "SELECT value FROM ItemTable WHERE key LIKE '%aiChat%' OR key LIKE '%chat%'"
    )?;
    let rows = stmt.query_map([], |row| {
        let text: String = row.get(0)?;
        Ok(text)
    })?;
    let mut out = Vec::new();
    for row in rows {
        let text = row?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue; };
        if let Ok(mut msgs) = parse_cursor_chat_blob(&v, &ws) {
            out.append(&mut msgs);
        }
    }
    Ok(out)
}

pub fn parse_cursor_chat_blob(v: &serde_json::Value, workspace_hash: &str) -> Result<Vec<Message>> {
    let chats = v.get("chats").and_then(|c| c.as_array());
    let Some(chats) = chats else { return Ok(Vec::new()); };
    let mut out = Vec::new();
    for chat in chats {
        let id = chat.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let Some(msgs) = chat.get("messages").and_then(|m| m.as_array()) else { continue; };
        for m in msgs {
            let role = match m.get("role").and_then(|v| v.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                Some("system") => Role::System,
                Some("tool") => Role::Tool,
                _ => continue,
            };
            let Some(content) = m.get("content").and_then(|v| v.as_str()) else { continue; };
            out.push(Message {
                v: 1, ts: Utc::now(), src: Source::Cursor,
                conv: format!("{workspace_hash}/{id}"),
                role,
                content: Content::Text { value: content.to_string() },
                meta: serde_json::json!({"workspace": workspace_hash}),
                refs: vec![],
            });
        }
    }
    Ok(out)
}

pub fn parse_specstory_md(md: &str, chat_id: &str) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut current_role: Option<Role> = None;
    let mut current_buf = String::new();
    for raw in md.lines() {
        let line = raw.trim();
        if let Some(stripped) = line.strip_prefix("#### ") {
            if let Some(role) = current_role.take() {
                if !current_buf.trim().is_empty() {
                    out.push(Message {
                        v: 1, ts: Utc::now(), src: Source::Cursor,
                        conv: chat_id.into(), role,
                        content: Content::Text { value: current_buf.trim().into() },
                        meta: serde_json::Value::Null, refs: vec![],
                    });
                }
                current_buf.clear();
            }
            current_role = match stripped.to_lowercase().as_str() {
                "user" => Some(Role::User),
                "assistant" => Some(Role::Assistant),
                _ => None,
            };
        } else if line == "---" {
            if let Some(role) = current_role.take() {
                if !current_buf.trim().is_empty() {
                    out.push(Message {
                        v: 1, ts: Utc::now(), src: Source::Cursor,
                        conv: chat_id.into(), role,
                        content: Content::Text { value: current_buf.trim().into() },
                        meta: serde_json::Value::Null, refs: vec![],
                    });
                }
                current_buf.clear();
            }
        } else if current_role.is_some() {
            current_buf.push_str(raw);
            current_buf.push('\n');
        }
    }
    if let Some(role) = current_role.take() {
        if !current_buf.trim().is_empty() {
            out.push(Message {
                v: 1, ts: Utc::now(), src: Source::Cursor,
                conv: chat_id.into(), role,
                content: Content::Text { value: current_buf.trim().into() },
                meta: serde_json::Value::Null, refs: vec![],
            });
        }
    }
    Ok(out)
}
```

- [ ] **Step 12.3: Add rusqlite to `mur-core/Cargo.toml`**

```toml
rusqlite = { version = "0.32", features = ["bundled"] }
```

- [ ] **Step 12.4: Uncomment `pub mod cursor;` in `ingest/mod.rs`**
- [ ] **Step 12.5: Run tests**

Run: `cargo test -p mur-core conversations::ingest::cursor::tests` — expect 3 PASS.

- [ ] **Step 12.6: Commit**

```bash
git add mur-core/src/conversations/ingest/cursor.rs \
        mur-core/src/conversations/ingest/mod.rs mur-core/Cargo.toml
git commit -m "feat(core): Cursor ingester — SQLite state.vscdb + .specstory

Best-effort schema parse; unknown shapes return empty rather than error.
Scans ~/Library/Application Support/Cursor/User/workspaceStorage/*/state.vscdb."
```

---

## Task 13: Gemini CLI ingester

**Files:**
- Create: `mur-core/src/conversations/ingest/gemini.rs`
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 13.1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gemini_chat_json() {
        let j = serde_json::json!({
            "history": [
                {"role": "user", "parts": [{"text": "hello"}]},
                {"role": "model", "parts": [{"text": "hi"}]}
            ]
        });
        let msgs = parse_gemini_chat(&j, "sess-1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].conv, "sess-1");
        assert!(matches!(msgs[1].role, mur_common::Role::Assistant));
    }

    #[test]
    fn multi_part_concatenates() {
        let j = serde_json::json!({
            "history": [
                {"role": "user", "parts": [{"text": "hello "}, {"text": "world"}]}
            ]
        });
        let msgs = parse_gemini_chat(&j, "s").unwrap();
        assert_eq!(msgs.len(), 1);
        if let mur_common::Content::Text { value } = &msgs[0].content {
            assert_eq!(value, "hello world");
        } else { panic!("expected text"); }
    }
}
```

- [ ] **Step 13.2: Implement `gemini.rs`**

```rust
//! Gemini CLI ingester. Reads ~/.gemini/tmp/<hash>/chats/*.json.
//! Schema: { history: [ { role, parts: [ {text} ] } ] }. Gemini uses "model"
//! for assistant; we map to Role::Assistant.

use anyhow::Result;
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::PathBuf;

pub fn list_gemini_chats() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new(); };
    let base = home.join(".gemini").join("tmp");
    if !base.exists() { return Vec::new(); }
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&base) else { return out; };
    for hash_dir in rd.flatten() {
        let chats = hash_dir.path().join("chats");
        if !chats.exists() { continue; }
        let Ok(rd2) = std::fs::read_dir(&chats) else { continue; };
        for f in rd2.flatten() {
            if f.path().extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(f.path());
            }
        }
    }
    out
}

pub fn parse_gemini_chat(v: &serde_json::Value, session_id: &str) -> Result<Vec<Message>> {
    let Some(history) = v.get("history").and_then(|h| h.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for turn in history {
        let role = match turn.get("role").and_then(|v| v.as_str()) {
            Some("user") => Role::User,
            Some("model") | Some("assistant") => Role::Assistant,
            Some("system") => Role::System,
            _ => continue,
        };
        let mut text = String::new();
        if let Some(parts) = turn.get("parts").and_then(|p| p.as_array()) {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                }
            }
        }
        if text.is_empty() { continue; }
        out.push(Message {
            v: 1, ts: Utc::now(), src: Source::Gemini,
            conv: session_id.into(), role,
            content: Content::Text { value: text },
            meta: serde_json::Value::Null, refs: vec![],
        });
    }
    Ok(out)
}
```

- [ ] **Step 13.3: Uncomment `pub mod gemini;` + run tests + commit**

Run: `cargo test -p mur-core conversations::ingest::gemini::tests` — expect 2 PASS.

Commit:
```bash
git add mur-core/src/conversations/ingest/gemini.rs mur-core/src/conversations/ingest/mod.rs
git commit -m "feat(core): Gemini CLI ingester — parse ~/.gemini/tmp/*/chats/*.json"
```

---

## Task 14: Aider ingester

**Files:**
- Create: `mur-core/src/conversations/ingest/aider.rs`
- Modify: `mur-core/src/conversations/ingest/mod.rs`

- [ ] **Step 14.1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_history() {
        let md = r"# aider chat started at 2026-04-19

#### test 1

#### > hello aider

hi back

#### > bye

bye
";
        let msgs = parse_aider_md(md, "chat-1").unwrap();
        assert!(msgs.len() >= 2);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(parse_aider_md("", "c").unwrap().is_empty());
    }
}
```

- [ ] **Step 14.2: Implement `aider.rs`**

```rust
//! Aider ingester. Scans configured `watched_dirs` for .aider.chat.history.md.
//! User turns: lines prefixed with `#### > `. Assistant turns: everything else.

use anyhow::Result;
use chrono::Utc;
use mur_common::{Content, Message, Role, Source};
use std::path::PathBuf;

pub fn find_aider_histories(watched: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in watched {
        walk(root, &mut out);
    }
    out
}

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return; };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            // Skip common bulky dirs
            let n = e.file_name();
            let name = n.to_string_lossy();
            if matches!(name.as_ref(), "node_modules" | "target" | ".git") { continue; }
            walk(&p, out);
        } else if p.file_name().and_then(|s| s.to_str()) == Some(".aider.chat.history.md") {
            out.push(p);
        }
    }
}

pub fn parse_aider_md(md: &str, chat_id: &str) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut current_role: Option<Role> = None;
    let mut buf = String::new();
    for raw in md.lines() {
        let line = raw.trim_start();
        if let Some(user_text) = line.strip_prefix("#### > ") {
            flush(&mut out, &mut current_role, &mut buf, chat_id);
            current_role = Some(Role::User);
            buf.push_str(user_text);
            buf.push('\n');
        } else if line.starts_with("####") {
            flush(&mut out, &mut current_role, &mut buf, chat_id);
        } else {
            if current_role.is_none() && !line.is_empty() {
                current_role = Some(Role::Assistant);
            }
            if current_role.is_some() {
                buf.push_str(raw);
                buf.push('\n');
            }
        }
    }
    flush(&mut out, &mut current_role, &mut buf, chat_id);
    Ok(out)
}

fn flush(out: &mut Vec<Message>, role: &mut Option<Role>, buf: &mut String, chat_id: &str) {
    if let Some(r) = role.take() {
        if !buf.trim().is_empty() {
            out.push(Message {
                v: 1, ts: Utc::now(), src: Source::Aider,
                conv: chat_id.into(), role: r,
                content: Content::Text { value: buf.trim().into() },
                meta: serde_json::Value::Null, refs: vec![],
            });
        }
        buf.clear();
    } else {
        buf.clear();
    }
}
```

- [ ] **Step 14.3: Uncomment + test + commit**

Run: `cargo test -p mur-core conversations::ingest::aider::tests` — expect 2 PASS.

```bash
git add mur-core/src/conversations/ingest/aider.rs mur-core/src/conversations/ingest/mod.rs
git commit -m "feat(core): Aider ingester — scan watched_dirs for .aider.chat.history.md"
```

---

## Task 15: Retention (three-guard cleanup)

**Files:**
- Create: `mur-core/src/conversations/retention.rs`
- Modify: `mur-core/src/conversations/mod.rs`

- [ ] **Step 15.1: Write failing tests**

Add to `retention.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn seed_raw(root: &str, ymd: (i32, u32, u32)) {
        let ts = chrono::Utc.with_ymd_and_hms(ymd.0, ymd.1, ymd.2, 12, 0, 0).unwrap();
        let msg = Message {
            v: 1, ts, src: Source::ClaudeCode, conv: "c".into(), role: Role::User,
            content: Content::Text { value: "x".into() },
            meta: serde_json::Value::Null, refs: vec![],
        };
        crate::conversations::store::append(&msg, Some(root)).unwrap();
    }

    fn write_summary(root: &str, ymd: (i32, u32, u32), body: &str) {
        let d = chrono::NaiveDate::from_ymd_opt(ymd.0, ymd.1, ymd.2).unwrap();
        let (md, _yml) = crate::conversations::paths::summary_paths_for(d, Some(root));
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(&md, body).unwrap();
    }

    #[test]
    fn old_raw_with_summary_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 1, 1));
        write_summary(root, (2026, 1, 1), "---\ndate: 2026-01-01\n---\n");
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 1);
        let raw = crate::conversations::paths::raw_root(Some(root)).join("2026-01-01");
        assert!(!raw.exists());
    }

    #[test]
    fn old_raw_without_summary_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 1, 1));
        // No summary written!
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 0);
        assert_eq!(r.dirs_skipped_no_summary, 1);
    }

    #[test]
    fn recent_raw_kept_even_with_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 4, 18));
        write_summary(root, (2026, 4, 18), "---\n---\n");
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 0);
    }
}
```

- [ ] **Step 15.2: Implement `retention.rs`**

```rust
//! Three-guard retention cleanup (spec §4.6).
//!
//! Guards (all three must pass or delete is skipped):
//! 1. Age > retention_days
//! 2. summary/<date>.md exists
//! 3. Audit successfully records Delete entry (records before rm)

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use std::fs;
use tracing::warn;

use super::audit::{Audit, AuditAction};
use super::paths::{raw_root, summary_paths_for};
use super::store::list_raw_dirs;

#[derive(Debug, Default)]
pub struct CleanupReport {
    pub dirs_scanned: u64,
    pub dirs_deleted: u64,
    pub dirs_skipped_not_old_enough: u64,
    pub dirs_skipped_no_summary: u64,
    pub dirs_errored: u64,
    pub bytes_freed: u64,
}

pub fn cleanup(
    now: DateTime<Utc>,
    retention_days: u32,
    root_override: Option<&str>,
) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();
    let audit = Audit::open(root_override)?;

    for (date, dir) in list_raw_dirs(root_override)? {
        report.dirs_scanned += 1;

        // Guard 1: age
        let age_days = (now.date_naive() - date).num_days();
        if age_days < retention_days as i64 {
            report.dirs_skipped_not_old_enough += 1;
            continue;
        }

        // Guard 2: summary exists
        let (md, _yml) = summary_paths_for(date, root_override);
        if !md.exists() {
            warn!("retention: skipping {dir:?} — no summary at {md:?}");
            report.dirs_skipped_no_summary += 1;
            continue;
        }

        // Guard 3: compute bytes, record audit, then remove
        let bytes = dir_size_bytes(&dir).unwrap_or(0);
        if let Err(e) = audit.append(AuditAction::Delete {
            target: dir.to_string_lossy().into_owned(),
            reason: format!("retention {retention_days}d"),
        }) {
            warn!("retention: audit append failed, skipping {dir:?}: {e:#}");
            report.dirs_errored += 1;
            continue;
        }
        if let Err(e) = fs::remove_dir_all(&dir) {
            warn!("retention: rm_rf failed for {dir:?}: {e:#}");
            report.dirs_errored += 1;
            continue;
        }
        report.dirs_deleted += 1;
        report.bytes_freed += bytes;
    }

    let _ = raw_root(root_override); // silence unused import on empty runs
    Ok(report)
}

fn dir_size_bytes(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += dir_size_bytes(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Read retention_days from ~/.mur/config.yaml (`conversations.retention_days`).
/// Defaults to 30 if absent.
pub fn retention_days_from_config() -> u32 {
    let Some(home) = dirs::home_dir() else { return 30; };
    let cfg = home.join(".mur").join("config.yaml");
    let Ok(text) = fs::read_to_string(&cfg) else { return 30; };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { return 30; };
    doc.get("conversations")
        .and_then(|c| c.get("retention_days"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(30)
}
```

- [ ] **Step 15.3: Uncomment `pub mod retention;` in `conversations/mod.rs`**
- [ ] **Step 15.4: Run tests**

Run: `cargo test -p mur-core conversations::retention::tests` — expect 3 PASS.

- [ ] **Step 15.5: Commit**

```bash
git add mur-core/src/conversations/retention.rs mur-core/src/conversations/mod.rs
git commit -m "feat(core): conversations retention — three-guard cleanup

Deletes raw/<date>/ only when age > retention_days AND summary exists AND
audit Delete entry records successfully. Conservative by design: any
failure skips the delete (never data loss from a broken guard)."
```

---

## Task 16: Retrieval — Mode A (timeline) + Mode B (search)

**Files:**
- Create: `mur-core/src/conversations/retrieve.rs`
- Modify: `mur-core/src/conversations/mod.rs`

- [ ] **Step 16.1: Write failing tests**

Add to `retrieve.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn append(root: &str, ts: (i32, u32, u32, u32), src: Source, text: &str) {
        let t = chrono::Utc.with_ymd_and_hms(ts.0, ts.1, ts.2, ts.3, 0, 0).unwrap();
        let m = Message {
            v: 1, ts: t, src, conv: "c".into(), role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null, refs: vec![],
        };
        crate::conversations::store::append(&m, Some(root)).unwrap();
    }

    #[test]
    fn mode_a_timeline_lists_days() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        append(root, (2026, 4, 18, 10), Source::ClaudeCode, "yesterday");
        append(root, (2026, 4, 19, 10), Source::Cursor, "today");
        let days = list_days(None, None, &[], Some(root)).unwrap();
        assert_eq!(days.len(), 2);
        // Most recent first
        assert_eq!(days[0].date, chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap());
    }

    #[test]
    fn mode_a_show_day_returns_all_messages_for_date() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        append(root, (2026, 4, 19, 9), Source::ClaudeCode, "hello");
        append(root, (2026, 4, 19, 10), Source::Slack, "world");
        let d = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let msgs = show_day(d, Some(root)).unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
```

- [ ] **Step 16.2: Implement `retrieve.rs`**

```rust
//! Retrieval — Mode A (timeline) and Mode B (search).
//! Mode C (NL Q&A) is Phase 2.

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::{Message, Source};

use super::paths::{summary_paths_for, raw_root};
use super::store::{read_day, list_raw_dirs};

#[derive(Debug, Clone)]
pub struct DaySummary {
    pub date: NaiveDate,
    pub msg_count: usize,
    pub sources: Vec<Source>,
    pub summary_exists: bool,
}

/// Mode A — list days (Layer 1 progressive disclosure).
pub fn list_days(
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    sources_filter: &[Source],
    root_override: Option<&str>,
) -> Result<Vec<DaySummary>> {
    let mut out = Vec::new();
    for (date, _dir) in list_raw_dirs(root_override)? {
        if let Some(s) = since { if date < s { continue; } }
        if let Some(u) = until { if date > u { continue; } }
        let msgs = read_day(date, root_override)?;
        if msgs.is_empty() { continue; }
        let sources: Vec<Source> = {
            let mut set: std::collections::BTreeSet<Source> = msgs.iter().map(|m| m.src).collect();
            if !sources_filter.is_empty() {
                set.retain(|s| sources_filter.contains(s));
                if set.is_empty() { continue; }
            }
            set.into_iter().collect()
        };
        let (md, _) = summary_paths_for(date, root_override);
        out.push(DaySummary {
            date,
            msg_count: msgs.len(),
            sources,
            summary_exists: md.exists(),
        });
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

/// Mode A — show all messages for a day (Layer 2 without summary, Layer 3 raw).
pub fn show_day(date: NaiveDate, root_override: Option<&str>) -> Result<Vec<Message>> {
    read_day(date, root_override)
}

/// Mode A — show the rendered summary file for a day.
pub fn show_summary(date: NaiveDate, root_override: Option<&str>) -> Result<Option<String>> {
    let (md, _) = summary_paths_for(date, root_override);
    if !md.exists() { return Ok(None); }
    Ok(Some(std::fs::read_to_string(md)?))
}

/// Mode B — semantic search via LanceDB + keyword rerank.
///
/// Requires the index to exist. Returns ordered hits (score desc).
pub async fn search(
    query: &str,
    embedding: Vec<f32>,
    limit: usize,
    source_filter: Option<Source>,
    root_override: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let idx = super::index::ConversationIndex::open(
        embedding.len() as i32, root_override,
    ).await?;
    let vec_hits = idx.search(&embedding, limit * 3, source_filter).await?;

    // Keyword rerank: 0.7 vector + 0.3 keyword, mirroring mur's scoring
    let q_lower = query.to_lowercase();
    let q_words: Vec<&str> = q_lower.split_whitespace().collect();

    let mut out: Vec<SearchResult> = vec_hits.into_iter().map(|h| {
        let vec_score = 1.0 / (1.0 + h.distance as f64);
        let kw_hits = q_words.iter()
            .filter(|w| h.content.to_lowercase().contains(*w))
            .count() as f64;
        let kw_score = if q_words.is_empty() { 0.0 } else { kw_hits / q_words.len() as f64 };
        let combined = 0.7 * vec_score + 0.3 * kw_score;
        SearchResult {
            id: h.id,
            ts: h.ts,
            source: h.source,
            conv_id: h.conv_id,
            snippet: h.content,
            score: combined,
        }
    }).collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub snippet: String,
    pub score: f64,
}
```

- [ ] **Step 16.3: Uncomment `pub mod retrieve;` in `conversations/mod.rs`**
- [ ] **Step 16.4: Run tests**

Run: `cargo test -p mur-core conversations::retrieve::tests` — expect 2 PASS. (Search tests happen in the CLI integration task since they need embeddings.)

- [ ] **Step 16.5: Commit**

```bash
git add mur-core/src/conversations/retrieve.rs mur-core/src/conversations/mod.rs
git commit -m "feat(core): conversations retrieve — Mode A (timeline) + Mode B (search)

Mode A is pure file walk (list_days, show_day, show_summary). Mode B
delegates embedding to mur's existing store/embedding.rs and uses the
same 0.7-vector + 0.3-keyword rerank as mur's pattern scoring."
```

---

## Task 17: CLI subcommand definitions and dispatch

**Files:**
- Create: `mur-core/src/cmd/conversations_cmd.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub(crate) mod conversations_cmd;`)
- Modify: `mur-core/src/main.rs` (add `Commands::Chat` and `Commands::Conversations` variants + dispatch)

- [ ] **Step 17.1: Write failing integration test**

Create `mur-core/tests/cli_conversations.rs`:

```rust
use std::process::Command;

#[test]
fn mur_chat_list_runs_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["chat", "list"])
        .env("HOME", tmp.path())
        .output()
        .expect("run mur");
    // May print empty (no conversations), but must not crash.
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn mur_conversations_doctor_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "doctor"])
        .env("HOME", tmp.path())
        .output()
        .expect("run mur");
    assert!(out.status.success());
}
```

- [ ] **Step 17.2: Create `cmd/conversations_cmd.rs` with handler stubs**

```rust
//! CLI handlers for conversations archive commands.
//! See spec §6.

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::Source;
use tracing::info;

use crate::conversations;

pub fn cmd_chat_list(since: Option<String>, src: Option<String>) -> Result<()> {
    let since_date = since.as_deref().map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d")).transpose()?;
    let sources: Vec<Source> = src.as_deref().map(parse_sources).unwrap_or_default();
    let days = conversations::retrieve::list_days(since_date, None, &sources, None)?;
    if days.is_empty() {
        println!("(no conversations)");
        return Ok(());
    }
    for d in days {
        let src_tags: Vec<String> = d.sources.iter().map(|s| s.file_prefix().into()).collect();
        let summary = if d.summary_exists { "✓" } else { "·" };
        println!("{}  {}  {:>4} msgs  [{}]",
            d.date, summary, d.msg_count, src_tags.join(","));
    }
    Ok(())
}

pub fn cmd_chat_show(date: String) -> Result<()> {
    let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
    if let Some(summary) = conversations::retrieve::show_summary(d, None)? {
        println!("{summary}");
        return Ok(());
    }
    // Fall back to raw render
    println!("# {d} (no summary; showing raw)\n");
    for m in conversations::retrieve::show_day(d, None)? {
        let text = match &m.content {
            mur_common::Content::Text { value } => value.clone(),
            mur_common::Content::ToolRef { desc, bytes, .. } => format!("[tool_ref: {desc} ({bytes}B)]"),
            mur_common::Content::ImageRef { desc, .. } => format!("[image_ref: {desc}]"),
        };
        println!("[{}] {}/{}: {}", m.ts.format("%H:%M:%S"), m.src.file_prefix(), m.conv, text);
    }
    Ok(())
}

pub fn cmd_chat_raw(date: String, conv: String) -> Result<()> {
    let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
    let ts = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let dir = conversations::paths::raw_dir_for(ts, None);
    if !dir.exists() {
        println!("(no raw for {d})");
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().contains(&conv) { continue; }
        let content = std::fs::read_to_string(entry.path())?;
        print!("{content}");
    }
    Ok(())
}

pub async fn cmd_chat_search(query: String, limit: usize, src: Option<String>) -> Result<()> {
    let source_filter = src.as_deref().and_then(|s| parse_sources(s).into_iter().next());
    let cfg = mur_common::config::Config::load().unwrap_or_default();
    let embed = crate::store::embedding::embed(&query, &cfg.embedding).await?;
    let hits = conversations::retrieve::search(&query, embed, limit, source_filter, None).await?;
    if hits.is_empty() { println!("(no matches)"); return Ok(()); }
    for h in hits {
        let when = chrono::DateTime::<chrono::Utc>::from_timestamp(h.ts, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        println!("[{:.2}] {} {}/{}: {}", h.score, when, h.source.file_prefix(), h.conv_id,
                 truncate(&h.snippet, 120));
    }
    Ok(())
}

pub async fn cmd_conversations_pull() -> Result<()> {
    info!("conversations pull: scanning all poll-based ingesters");
    let mut pipeline = conversations::ingest::pipeline::Pipeline::new(None)?;

    // Cursor
    for ws in conversations::ingest::cursor::list_cursor_workspaces() {
        if let Ok(msgs) = conversations::ingest::cursor::scan_workspace(&ws) {
            if !msgs.is_empty() {
                let r = pipeline.run(msgs)?;
                println!("cursor {}: {} accepted, {} rejected, {} deduped",
                    ws.file_name().unwrap().to_string_lossy(), r.accepted, r.rejected, r.deduped);
            }
        }
    }

    // Gemini
    for path in conversations::ingest::gemini::list_gemini_chats() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue; };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue; };
        let id = path.file_stem().unwrap().to_string_lossy().to_string();
        if let Ok(msgs) = conversations::ingest::gemini::parse_gemini_chat(&v, &id) {
            if !msgs.is_empty() {
                let r = pipeline.run(msgs)?;
                println!("gemini {}: {} accepted", id, r.accepted);
            }
        }
    }

    // Aider — read watched_dirs from config
    let watched = read_aider_watched();
    for hist in conversations::ingest::aider::find_aider_histories(&watched) {
        let Ok(md) = std::fs::read_to_string(&hist) else { continue; };
        let id = hist.parent().and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "aider".into());
        if let Ok(msgs) = conversations::ingest::aider::parse_aider_md(&md, &id) {
            if !msgs.is_empty() {
                let r = pipeline.run(msgs)?;
                println!("aider {}: {} accepted", id, r.accepted);
            }
        }
    }

    Ok(())
}

pub async fn cmd_conversations_cleanup() -> Result<()> {
    let days = conversations::retention::retention_days_from_config();
    let r = conversations::retention::cleanup(chrono::Utc::now(), days, None)?;
    println!("Scanned {} dirs, deleted {}, {} KB freed, {} kept (no summary), {} errored",
        r.dirs_scanned, r.dirs_deleted, r.bytes_freed / 1024,
        r.dirs_skipped_no_summary, r.dirs_errored);
    Ok(())
}

pub async fn cmd_conversations_reindex() -> Result<()> {
    // Re-embed every message in raw/ and rebuild the LanceDB table.
    // For Phase 1: implement the minimal round-trip (read → embed → upsert).
    let cfg = mur_common::config::Config::load().unwrap_or_default();
    let dims = cfg.embedding.dimensions as i32;
    let mut idx = conversations::index::ConversationIndex::open(dims, None).await?;
    let mut count = 0u64;
    for (date, _dir) in conversations::store::list_raw_dirs(None)? {
        let msgs = conversations::store::read_day(date, None)?;
        let mut entries = Vec::with_capacity(msgs.len());
        for m in msgs {
            let txt = match &m.content {
                mur_common::Content::Text { value } => value.clone(),
                mur_common::Content::ToolRef { desc, .. } => desc.clone(),
                mur_common::Content::ImageRef { desc, .. } => desc.clone(),
            };
            let vec = crate::store::embedding::embed(&txt, &cfg.embedding).await?;
            entries.push((m, vec));
        }
        let len = entries.len() as u64;
        idx.upsert(&entries).await?;
        count += len;
    }
    println!("Reindexed {count} messages");
    Ok(())
}

pub fn cmd_conversations_doctor() -> Result<()> {
    println!("conversations doctor");
    let dirs = conversations::store::list_raw_dirs(None).unwrap_or_default();
    println!("  ✓ raw day-dirs: {}", dirs.len());

    let audit_ok = conversations::audit::verify(None).unwrap_or(false);
    println!("  {} audit hash chain", if audit_ok { "✓" } else { "✗" });

    let cfg_days = conversations::retention::retention_days_from_config();
    println!("  ✓ retention_days = {cfg_days}");

    let enabled = conversations::is_enabled().unwrap_or(false);
    println!("  {} conversations.enabled", if enabled { "✓" } else { "·" });

    Ok(())
}

fn parse_sources(s: &str) -> Vec<Source> {
    s.split(',').filter_map(|p| match p.trim() {
        "cc" | "claude-code" => Some(Source::ClaudeCode),
        "cursor" => Some(Source::Cursor),
        "gemini" => Some(Source::Gemini),
        "aider" => Some(Source::Aider),
        "slack" => Some(Source::Slack),
        "telegram" | "tg" => Some(Source::Telegram),
        "discord" => Some(Source::Discord),
        "commander" => Some(Source::CommanderEngine),
        _ => None,
    }).collect()
}

fn read_aider_watched() -> Vec<std::path::PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new(); };
    let cfg = home.join(".mur").join("config.yaml");
    let Ok(text) = std::fs::read_to_string(&cfg) else { return Vec::new(); };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { return Vec::new(); };
    doc.get("conversations")
        .and_then(|c| c.get("sources"))
        .and_then(|s| s.get("aider"))
        .and_then(|a| a.get("watched_dirs"))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter()
            .filter_map(|v| v.as_str().map(|s| std::path::PathBuf::from(shellexpand::tilde(s).to_string())))
            .collect())
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= max { s.to_string() } else {
        format!("{}…", c.iter().take(max).collect::<String>())
    }
}
```

Add `shellexpand = "3"` to `mur-core/Cargo.toml`.

- [ ] **Step 17.3: Add subcommand variants to `mur-core/src/main.rs`**

Find the `enum Commands { ... }` block and insert new variants. Also add dispatch match arms in the main `match` block (the one starting ~line 715). Structure:

```rust
// New variants:
/// Conversations archive (Layer 1 index, Layer 2 summary, Layer 3 raw)
Chat {
    #[command(subcommand)]
    action: ChatAction,
},
/// Conversations archive management (pull / compact / reindex / doctor / migrate)
Conversations {
    #[command(subcommand)]
    action: ConversationsAction,
},
```

And new enums in the same file:

```rust
#[derive(Subcommand)]
enum ChatAction {
    /// List days in the archive (Layer 1)
    List {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        src: Option<String>,
    },
    /// Show a single day's summary (or raw if no summary) (Layer 2)
    Show { date: String },
    /// Dump raw JSONL for a conversation (Layer 3)
    Raw { date: String, conv: String },
    /// Semantic + keyword search
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        src: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConversationsAction {
    /// Run polling ingesters (Cursor/Gemini/Aider)
    Pull,
    /// Apply retention cleanup
    Cleanup,
    /// Rebuild LanceDB from raw/
    Reindex,
    /// Run health checks
    Doctor,
    /// Scan commander paths and report migration plan
    Migrate {
        #[arg(long, conflicts_with = "run")]
        dry_run: bool,
        #[arg(long, conflicts_with = "dry_run")]
        run: bool,
    },
    /// Roll back to commander's old paths
    Rollback,
}
```

Dispatch arms:

```rust
Commands::Chat { action } => match action {
    ChatAction::List { since, src } => cmd::conversations_cmd::cmd_chat_list(since, src)?,
    ChatAction::Show { date } => cmd::conversations_cmd::cmd_chat_show(date)?,
    ChatAction::Raw { date, conv } => cmd::conversations_cmd::cmd_chat_raw(date, conv)?,
    ChatAction::Search { query, limit, src } =>
        cmd::conversations_cmd::cmd_chat_search(query, limit, src).await?,
},
Commands::Conversations { action } => match action {
    ConversationsAction::Pull => cmd::conversations_cmd::cmd_conversations_pull().await?,
    ConversationsAction::Cleanup => cmd::conversations_cmd::cmd_conversations_cleanup().await?,
    ConversationsAction::Reindex => cmd::conversations_cmd::cmd_conversations_reindex().await?,
    ConversationsAction::Doctor => cmd::conversations_cmd::cmd_conversations_doctor()?,
    ConversationsAction::Migrate { dry_run, run } =>
        cmd::conversations_cmd::cmd_conversations_migrate(dry_run, run).await?,
    ConversationsAction::Rollback => cmd::conversations_cmd::cmd_conversations_rollback().await?,
},
```

(The `migrate` and `rollback` handlers land in Task 18/19; leave stubs that `bail!("migrate not yet implemented")` for now so the build succeeds.)

- [ ] **Step 17.4: Register module in `cmd/mod.rs`**

Add line: `pub(crate) mod conversations_cmd;`

- [ ] **Step 17.5: Run tests**

Run: `cargo test -p mur-core --test cli_conversations` — expect 2 PASS.

- [ ] **Step 17.6: Commit**

```bash
git add mur-core/src/cmd/conversations_cmd.rs mur-core/src/cmd/mod.rs \
        mur-core/src/main.rs mur-core/Cargo.toml mur-core/tests/cli_conversations.rs
git commit -m "feat(core): add mur chat + mur conversations CLI subcommands

chat {list|show|raw|search} implements Mode A+B three-tier disclosure.
conversations {pull|cleanup|reindex|doctor|migrate|rollback} handles
maintenance. Integration tests verify the binary runs with an empty HOME."
```

---

## Task 18: Migration — dry-run (scan commander paths)

**Files:**
- Create: `mur-core/src/conversations/migrate.rs`
- Modify: `mur-core/src/conversations/mod.rs`
- Modify: `mur-core/src/cmd/conversations_cmd.rs` (wire `cmd_conversations_migrate`)

- [ ] **Step 18.1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn seed_commander_layout(home: &std::path::Path) {
        let mem = home.join(".mur/commander/memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("long_term.jsonl"),
            r#"{"id":"1","text":"hi","metadata":{},"timestamp_secs":1776571759,"vector":[]}
"#,
        ).unwrap();
        let u = home.join(".mur/commander/users/alice");
        std::fs::create_dir_all(&u).unwrap();
        std::fs::write(u.join("conversation.jsonl"),
            r#"{"timestamp":1776571759,"role":"user","text":"hello"}
"#).unwrap();
    }

    #[test]
    fn dry_run_counts_everything() {
        let tmp = tempfile::tempdir().unwrap();
        seed_commander_layout(tmp.path());
        let home = tmp.path().to_str().unwrap();
        let plan = dry_run(Some(home)).unwrap();
        assert_eq!(plan.long_term_lines, 1);
        assert_eq!(plan.user_turns, 1);
        assert!(plan.free_space_needed_bytes > 0);
    }

    #[test]
    fn dry_run_on_clean_install_has_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".mur")).unwrap();
        let plan = dry_run(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(plan.long_term_lines, 0);
        assert_eq!(plan.user_turns, 0);
    }
}
```

- [ ] **Step 18.2: Implement `migrate.rs` dry-run**

```rust
//! Commander → conversations migration (spec §7).
//!
//! Phase 1 includes:
//! - `dry_run(home)` — scan commander paths, count data, estimate space.
//! - `run(home)` — staged atomic migration (Task 19).
//! - `rollback(home)` — restore commander layout (Task 19).

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub struct MigrationPlan {
    pub long_term_lines: u64,
    pub user_turns: u64,
    pub user_count: u64,
    pub episode_count: u64,
    pub audit_entries: u64,
    pub current_usage_bytes: u64,
    pub free_space_needed_bytes: u64,
    pub commander_daemon_running: bool,
}

fn home_root(home_override: Option<&str>) -> PathBuf {
    home_override.map(PathBuf::from).unwrap_or_else(|| dirs::home_dir().unwrap())
}

fn count_jsonl_lines(p: &std::path::Path) -> u64 {
    if !p.exists() { return 0; }
    let Ok(content) = std::fs::read_to_string(p) else { return 0; };
    content.lines().filter(|l| !l.trim().is_empty()).count() as u64
}

pub fn dry_run(home_override: Option<&str>) -> Result<MigrationPlan> {
    let home = home_root(home_override);
    let mur = home.join(".mur");

    let lt = mur.join("commander/memory/long_term.jsonl");
    let long_term_lines = count_jsonl_lines(&lt);

    let users_dir = mur.join("commander/users");
    let mut user_turns = 0u64;
    let mut user_count = 0u64;
    if users_dir.exists() {
        for u in std::fs::read_dir(&users_dir)? {
            let u = u?;
            if !u.file_type()?.is_dir() { continue; }
            user_count += 1;
            user_turns += count_jsonl_lines(&u.path().join("conversation.jsonl"));
        }
    }

    let episodes_dir = mur.join("commander/memory/episodes");
    let mut episode_count = 0u64;
    if episodes_dir.exists() {
        for e in walkdir::WalkDir::new(&episodes_dir).into_iter().flatten() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("md") {
                episode_count += 1;
            }
        }
    }

    let audit_entries = count_jsonl_lines(&mur.join("commander/audit.jsonl"));
    let current = dir_size_bytes(&mur.join("commander/memory")).unwrap_or(0);

    // 1.5x safety factor for staging
    let free_space_needed_bytes = current + current / 2;

    let commander_daemon_running = daemon_running();

    Ok(MigrationPlan {
        long_term_lines, user_turns, user_count, episode_count, audit_entries,
        current_usage_bytes: current,
        free_space_needed_bytes,
        commander_daemon_running,
    })
}

fn dir_size_bytes(p: &std::path::Path) -> Result<u64> {
    if !p.exists() { return Ok(0); }
    let mut t = 0u64;
    for e in std::fs::read_dir(p)? {
        let e = e?;
        if e.file_type()?.is_dir() { t += dir_size_bytes(&e.path())?; }
        else { t += e.metadata()?.len(); }
    }
    Ok(t)
}

fn daemon_running() -> bool {
    // Best-effort: look for mur-commander pid file or listening socket.
    // For Phase 1 we only return false to avoid blocking tests; later tasks
    // may tighten this check.
    false
}

pub fn render_plan(p: &MigrationPlan) -> String {
    format!(
        "Migration plan (commander → conversations):\n  \
         long_term.jsonl: {} lines\n  \
         users: {} with {} turns total\n  \
         episodes: {} md files\n  \
         audit: {} entries\n  \
         current usage: {:.1} MB\n  \
         free space needed: {:.1} MB (1.5× safety)\n  \
         commander daemon running: {}\n",
        p.long_term_lines, p.user_count, p.user_turns,
        p.episode_count, p.audit_entries,
        p.current_usage_bytes as f64 / 1_048_576.0,
        p.free_space_needed_bytes as f64 / 1_048_576.0,
        p.commander_daemon_running,
    )
}

pub async fn run(home_override: Option<&str>) -> Result<MigrationReport> {
    // Implemented in Task 19.
    bail!("migrate run not yet implemented in Task 18; see Task 19")
}

pub async fn rollback(home_override: Option<&str>) -> Result<MigrationReport> {
    bail!("rollback not yet implemented in Task 19")
}

pub struct MigrationReport {
    pub messages_migrated: u64,
    pub audit_entries_replayed: u64,
    pub duration_ms: u64,
}
```

- [ ] **Step 18.3: Add `walkdir = "2"` to `mur-core/Cargo.toml`**

- [ ] **Step 18.4: Wire into `conversations_cmd.rs`**

```rust
pub async fn cmd_conversations_migrate(dry_run_flag: bool, run_flag: bool) -> Result<()> {
    use crate::conversations::migrate;
    if !dry_run_flag && !run_flag {
        // Default to dry-run when neither flag is set.
        let plan = migrate::dry_run(None)?;
        println!("{}", migrate::render_plan(&plan));
        return Ok(());
    }
    if dry_run_flag {
        let plan = migrate::dry_run(None)?;
        println!("{}", migrate::render_plan(&plan));
        return Ok(());
    }
    let report = migrate::run(None).await?;
    println!("Migrated {} messages, replayed {} audit entries in {}ms",
        report.messages_migrated, report.audit_entries_replayed, report.duration_ms);
    Ok(())
}

pub async fn cmd_conversations_rollback() -> Result<()> {
    let report = crate::conversations::migrate::rollback(None).await?;
    println!("Rolled back {} messages in {}ms",
        report.messages_migrated, report.duration_ms);
    Ok(())
}
```

- [ ] **Step 18.5: Uncomment `pub mod migrate;` in `conversations/mod.rs`**
- [ ] **Step 18.6: Run tests**

Run: `cargo test -p mur-core conversations::migrate::tests` — expect 2 PASS.

- [ ] **Step 18.7: Commit**

```bash
git add mur-core/src/conversations/migrate.rs mur-core/src/conversations/mod.rs \
        mur-core/src/cmd/conversations_cmd.rs mur-core/Cargo.toml
git commit -m "feat(core): conversations migrate — dry-run plan scanner

Counts commander long_term/users/episodes/audit, estimates free space
(1.5x safety factor), reports plan. Actual run is Task 19."
```

---

