# Unified Channel Foundation (v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give MUR a single durable `Channel` object — goal + A2A-standard state + participants + an append-only event stream — stored once under `~/.mur/channels/` and shared by both the Hub and `mur agent cli`, so the two surfaces read and write the same conversation, with live updates and a one-shot importer for existing CLI sessions.

**Architecture:** Types live in `mur-common::channel` (pure types, no I/O — per CLAUDE.md). Store logic lives in a new lightweight crate `mur-channel` (event-sourced JSONL log + atomic YAML manifest + a rebuildable SQLite query index + a `notify` file-watcher), depended on by both `mur-core` (CLI) and `mur-hub-gui/src-tauri` (Hub). The JSONL event log is the single source of truth; SQLite is a droppable/rebuildable read-model for the "list / my work" view.

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json`/`serde_yaml`, `chrono`, `uuid` (v7), `rusqlite` 0.32 (bundled — already a `mur-core` dep), `notify` 6 (already a `mur-core` dep), `fs2` (advisory file lock), `tempfile` (tests), `cargo nextest`. Hub frontend is React + TypeScript + Tauri 2.

**Scope note (refinement of the spec's v1):** v1 ships the event log + the **SQLite** query index (both work fully offline). The spec's "LanceDB semantic index" for channels requires an embedding provider, which the existing `mur internals reindex` pipeline already owns; wiring channel events into that batch pipeline is deferred to a follow-up and is **not** on v1's critical path. Out of scope entirely: Hub "Work" view (v2), `channel/delegate` + dual-mode executor + HITL (v3), iOS (v4).

**Naming note:** the participant/event actor type is named `ChannelActor` to avoid colliding with the pre-existing `mur_common::actor::Actor` re-export.

---

## File Structure

**Created:**
- `mur-common/src/channel.rs` — all `Channel`/`ChannelEvent` types + a `ChannelActor::local_human()` helper. Pure types.
- `mur-channel/Cargo.toml`, `mur-channel/src/lib.rs` — new crate root.
- `mur-channel/src/store.rs` — `ChannelStore`: event-log JSONL + manifest YAML, atomic writes, append with monotonic `seq` under an advisory lock.
- `mur-channel/src/index.rs` — `ChannelIndex`: SQLite read-model (open/migrate/upsert/list/rebuild).
- `mur-channel/src/service.rs` — `ChannelService`: composes store+index behind one API the surfaces call.
- `mur-channel/src/watch.rs` — `watch_channels()`: `notify` recursive watcher → per-channel-id callback.
- `mur-core/src/cmd/agent/channel_import.rs` — one-shot importer from `cli-sessions/*.jsonl`.

**Modified:**
- `mur-common/src/lib.rs` — register `pub mod channel;` + re-exports.
- `Cargo.toml` (workspace root) — add `mur-channel` member; promote `notify`/`rusqlite` to `[workspace.dependencies]`.
- `mur-core/Cargo.toml` — depend on `mur-channel`.
- `mur-core/src/cmd/agent/cli/persist.rs` — back `Session` with `ChannelService` (full rewrite, same public surface).
- `mur-core/src/cmd/agent/cli/mod.rs:133-166` — `build_app` resume reads channels.
- `mur-core/src/dispatch.rs` — add `InternalsAction::MigrateChannels` arm + the CLI flag.
- `mur-hub-gui/src-tauri/Cargo.toml` — depend on `mur-channel` + `mur-common`.
- `mur-hub-gui/src-tauri/src/chat.rs` — persist turns to the channel store; add `channel_load`.
- `mur-hub-gui/src-tauri/src/lib.rs` — register `channel_load`; spawn the watcher emitting `channel-updated`.
- `mur-hub-gui/ui/src/components/ChatTab.tsx` — hydrate from channel on mount + on `channel-updated`.

---

## Task 1: `Channel` types in `mur-common`

**Files:**
- Create: `mur-common/src/channel.rs`
- Modify: `mur-common/src/lib.rs` (module decl + re-exports)
- Test: inline `#[cfg(test)]` in `mur-common/src/channel.rs`

- [ ] **Step 1: Write the failing test** — create `mur-common/src/channel.rs` with the types and a round-trip test.

```rust
//! Unified Channel — the single durable work object shared across surfaces.
//! Pure types only (no I/O); store logic lives in the `mur-channel` crate.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version for the manifest + event log; breaking changes bump this.
pub const CHANNEL_SCHEMA_VERSION: u32 = 1;

/// A2A v0.3 lifecycle, serialized on the wire as kebab-case (`input-required`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
    Stale,
}

/// Who produced an event / is a participant. Named `ChannelActor` to avoid
/// colliding with the pre-existing `mur_common::actor::Actor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChannelActor {
    Human { name: String },
    Agent { id: String },
    System,
}

impl ChannelActor {
    /// The local human owner, from `$USER`/`$USERNAME`, falling back to `you`.
    pub fn local_human() -> Self {
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "you".to_string());
        ChannelActor::Human { name }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    Owner,
    Router,
    Delegate,
    Observer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub actor: ChannelActor,
    pub role: ParticipantRole,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Goal {
    #[serde(default)]
    pub statement: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

/// The durable manifest (a cache of state derivable from the event log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub v: u32,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub goal: Goal,
    pub state: ChannelState,
    pub owner: ChannelActor,
    #[serde(default)]
    pub participants: Vec<Participant>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Message,
    Delegation,
    Handoff,
    ToolCall,
    ToolResult,
    StateChange,
    Artifact,
    HitlRequest,
    HitlResponse,
    Note,
}

/// One append-only line in `~/.mur/channels/<id>/events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub actor: ChannelActor,
    pub kind: EventKind,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_state_serializes_kebab() {
        let j = serde_json::to_string(&ChannelState::InputRequired).unwrap();
        assert_eq!(j, "\"input-required\"");
    }

    #[test]
    fn event_round_trips() {
        let ev = ChannelEvent {
            seq: 3,
            ts: Utc::now(),
            actor: ChannelActor::Agent { id: "qa".into() },
            kind: EventKind::Message,
            payload: serde_json::json!({ "text": "hello", "task_id": "t-1" }),
            idempotency_key: None,
        };
        let line = serde_json::to_string(&ev).unwrap();
        let back: ChannelEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.seq, 3);
        assert_eq!(back.actor, ChannelActor::Agent { id: "qa".into() });
        assert_eq!(back.payload["text"], "hello");
    }
}
```

Then register the module in `mur-common/src/lib.rs`. Add `pub mod channel;` between `pub mod canonical;` and `pub mod companion;`, and add this re-export line in the `pub use` block (after the `pub use a2a::...` lines):

```rust
pub use channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelState, EventKind, Goal,
    Participant, ParticipantRole,
};
```

- [ ] **Step 2: Run the test to verify it fails (not yet compiled in)**

Run: `cargo nextest run -p mur-common channel`
Expected: build error or no tests found until `pub mod channel;` is added; once added, tests compile.

- [ ] **Step 3: (covered in Step 1)** — the implementation is the type definitions written above.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p mur-common channel`
Expected: PASS — `channel_state_serializes_kebab`, `event_round_trips`.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/channel.rs mur-common/src/lib.rs
git commit -m "feat(channel): Channel/ChannelEvent types in mur-common"
```

---

## Task 2: `mur-channel` crate + `ChannelStore` (event log + manifest)

**Files:**
- Create: `mur-channel/Cargo.toml`, `mur-channel/src/lib.rs`, `mur-channel/src/store.rs`
- Modify: `Cargo.toml` (workspace root: members + workspace deps)
- Test: inline `#[cfg(test)]` in `mur-channel/src/store.rs`

- [ ] **Step 1: Create the crate manifest and workspace wiring**

In the root `/Volumes/Firecuda4tb/Projects/mur/Cargo.toml`, add `"mur-channel",` to the `[workspace] members = [...]` array (verified: insert it right after `"mur-compress",`), and add these two lines to `[workspace.dependencies]` (verified: `fs2` is already there; `tempfile` is not, hence the dev-dep below pins it directly):

```toml
notify = "6"
rusqlite = { version = "0.32", features = ["bundled"] }
```

Create `mur-channel/Cargo.toml`:

```toml
[package]
name = "mur-channel"
version.workspace = true
edition.workspace = true

[dependencies]
mur-common = { path = "../mur-common" }
serde = { workspace = true }
serde_json = { workspace = true }
serde_yaml = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
anyhow = { workspace = true }
rusqlite = { workspace = true }
notify = { workspace = true }
fs2 = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

Create `mur-channel/src/lib.rs`:

```rust
//! Unified Channel store: event-sourced JSONL log (source of truth) + a
//! rebuildable SQLite query index + a file-watcher. Shared by the CLI
//! (`mur-core`) and the Hub (`mur-hub-gui`).
pub mod index;
pub mod service;
pub mod store;
pub mod watch;

pub use service::ChannelService;
pub use store::ChannelStore;
```

- [ ] **Step 2: Write the failing test** — create `mur-channel/src/store.rs`:

```rust
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use mur_common::channel::{Channel, ChannelActor, ChannelEvent, EventKind};

/// Event-sourced store rooted at `<mur_home>/channels/`.
pub struct ChannelStore {
    root: PathBuf,
}

impl ChannelStore {
    pub fn new(mur_home: &Path) -> Self {
        Self { root: mur_home.join("channels") }
    }

    fn channel_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    fn events_path(&self, id: &str) -> PathBuf {
        self.channel_dir(id).join("events.jsonl")
    }
    fn manifest_path(&self, id: &str) -> PathBuf {
        self.channel_dir(id).join("channel.yaml")
    }

    /// Create the channel directory and write its initial manifest.
    pub fn create(&self, channel: &Channel) -> Result<()> {
        let dir = self.channel_dir(&channel.id);
        fs::create_dir_all(&dir).with_context(|| format!("create channel dir {}", dir.display()))?;
        self.save_manifest(channel)
    }

    /// Atomic manifest write (temp file + rename), matching the YamlStore idiom.
    pub fn save_manifest(&self, channel: &Channel) -> Result<()> {
        let path = self.manifest_path(&channel.id);
        let yaml = serde_yaml::to_string(channel).context("serialize channel manifest")?;
        let tmp = path.with_extension("yaml.tmp");
        fs::write(&tmp, &yaml).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    pub fn load_manifest(&self, id: &str) -> Result<Channel> {
        let path = self.manifest_path(id);
        let s = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_yaml::from_str(&s).with_context(|| format!("parse {}", path.display()))
    }

    pub fn load_events(&self, id: &str) -> Result<Vec<ChannelEvent>> {
        let path = self.events_path(id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<ChannelEvent>(l).ok())
            .collect())
    }

    /// Append one event under an advisory lock so `seq` stays monotonic across
    /// processes (the Hub and the CLI may append concurrently). Returns the
    /// event with its assigned `seq` and timestamp.
    pub fn append_event(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let dir = self.channel_dir(id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.events_path(id);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.lock_exclusive().context("lock events file")?;

        // Compute next seq from the existing log tail (held under the lock).
        let next_seq = self
            .load_events(id)
            .unwrap_or_default()
            .last()
            .map(|e| e.seq + 1)
            .unwrap_or(0);

        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
        };
        let line = serde_json::to_string(&ev).context("serialize event")?;
        writeln!(file, "{line}").with_context(|| format!("write {}", path.display()))?;
        FileExt::unlock(&file).ok();
        Ok(ev)
    }

    /// List every channel id present on disk.
    pub fn list_ids(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.root).with_context(|| format!("read {}", self.root.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{ChannelState, Goal};
    use tempfile::TempDir;

    fn sample_channel(id: &str) -> Channel {
        let now = Utc::now();
        Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: id.to_string(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            owner: ChannelActor::Human { name: "me".into() },
            participants: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn create_then_load_manifest() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let got = store.load_manifest("c1").unwrap();
        assert_eq!(got.id, "c1");
        assert_eq!(got.state, ChannelState::Working);
    }

    #[test]
    fn append_assigns_monotonic_seq() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let e0 = store
            .append_event("c1", ChannelActor::Human { name: "me".into() }, EventKind::Message, serde_json::json!({"text":"hi"}), None)
            .unwrap();
        let e1 = store
            .append_event("c1", ChannelActor::Agent { id: "qa".into() }, EventKind::Message, serde_json::json!({"text":"yo"}), None)
            .unwrap();
        assert_eq!(e0.seq, 0);
        assert_eq!(e1.seq, 1);
        let all = store.load_events("c1").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].payload["text"], "yo");
    }

    #[test]
    fn list_ids_returns_created() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("a")).unwrap();
        store.create(&sample_channel("b")).unwrap();
        let mut ids = store.list_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }
}
```

Note: `mur-channel/src/lib.rs` references `index`, `service`, `watch` modules created in later tasks. To compile Task 2 alone, temporarily comment out those three `pub mod`/`pub use` lines, or create empty stub files (`echo "" > mur-channel/src/{index,service,watch}.rs`). The stubs are filled in Tasks 3–5.

- [ ] **Step 3: Run the test to verify it fails first** (before writing bodies — here the bodies are written together, so this step verifies the crate compiles and tests run).

Run: `cargo nextest run -p mur-channel store`
Expected: PASS once the crate builds. If you staged the test before the impl, expect FAIL "cannot find function".

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-channel store`
Expected: PASS — `create_then_load_manifest`, `append_assigns_monotonic_seq`, `list_ids_returns_created`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml mur-channel/Cargo.toml mur-channel/src/lib.rs mur-channel/src/store.rs
git commit -m "feat(channel): mur-channel crate + event-sourced ChannelStore"
```

---

## Task 3: `ChannelIndex` (rebuildable SQLite read-model)

**Files:**
- Create/replace: `mur-channel/src/index.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test** — write `mur-channel/src/index.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::Channel;
use rusqlite::Connection;

use crate::store::ChannelStore;

/// SQLite read-model at `<mur_home>/index/channels.db`. Droppable & rebuildable
/// from the event-log manifests — never the source of truth.
pub struct ChannelIndex {
    conn: Connection,
}

/// One row of the channel-list / "my work" query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    pub id: String,
    pub title: String,
    pub state: String,
    pub updated_at: String,
}

impl ChannelIndex {
    pub fn open(mur_home: &Path) -> Result<Self> {
        let dir = mur_home.join("index");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let conn = Connection::open(dir.join("channels.db")).context("open channels.db")?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channels (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                state       TEXT NOT NULL,
                owner       TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_updated ON channels(updated_at DESC);",
        )?;
        Ok(())
    }

    pub fn upsert(&self, ch: &Channel) -> Result<()> {
        let owner = serde_json::to_string(&ch.owner)?;
        self.conn.execute(
            "INSERT INTO channels (id,title,state,owner,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, state=excluded.state,
               owner=excluded.owner, updated_at=excluded.updated_at",
            rusqlite::params![
                ch.id,
                ch.title,
                serde_json::to_string(&ch.state)?.trim_matches('"'),
                owner,
                ch.created_at.to_rfc3339(),
                ch.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Newest-first channel list (the Hub left-rail / CLI "my work" inbox).
    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,state,updated_at FROM channels ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(ChannelRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    state: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drop every row and re-derive from the store's manifests.
    pub fn rebuild_from(&self, store: &ChannelStore) -> Result<usize> {
        self.conn.execute("DELETE FROM channels", [])?;
        let mut n = 0;
        for id in store.list_ids()? {
            if let Ok(ch) = store.load_manifest(&id) {
                self.upsert(&ch)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::{ChannelActor, ChannelState, Goal};
    use tempfile::TempDir;

    fn ch(id: &str, state: ChannelState) -> Channel {
        let now = Utc::now();
        Channel {
            v: 1,
            id: id.into(),
            title: id.into(),
            goal: Goal::default(),
            state,
            owner: ChannelActor::Human { name: "me".into() },
            participants: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn upsert_and_list_newest_first() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        idx.upsert(&ch("a", ChannelState::Working)).unwrap();
        idx.upsert(&ch("b", ChannelState::Completed)).unwrap();
        let rows = idx.list(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].state, "completed"); // serialized kebab, quotes trimmed
    }

    #[test]
    fn rebuild_from_store_repopulates() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&ch("a", ChannelState::Working)).unwrap();
        store.create(&ch("b", ChannelState::Failed)).unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        assert_eq!(idx.list(10).unwrap().len(), 0);
        let n = idx.rebuild_from(&store).unwrap();
        assert_eq!(n, 2);
        assert_eq!(idx.list(10).unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails** (if impl staged after test)

Run: `cargo nextest run -p mur-channel index`
Expected: FAIL "cannot find type `ChannelIndex`" before the impl exists.

- [ ] **Step 3: (impl written in Step 1)**

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p mur-channel index`
Expected: PASS — `upsert_and_list_newest_first`, `rebuild_from_store_repopulates`.

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/index.rs
git commit -m "feat(channel): rebuildable SQLite ChannelIndex read-model"
```

---

## Task 4: `ChannelService` (one API over store + index)

**Files:**
- Create/replace: `mur-channel/src/service.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test** — write `mur-channel/src/service.rs`:

```rust
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelState, EventKind, Goal,
    Participant, ParticipantRole,
};

use crate::index::{ChannelIndex, ChannelRow};
use crate::store::ChannelStore;

/// The single API both the CLI and the Hub call. Keeps the log + the index in
/// sync on every mutation.
pub struct ChannelService {
    store: ChannelStore,
    index: ChannelIndex,
}

impl ChannelService {
    pub fn open(mur_home: &Path) -> Result<Self> {
        Ok(Self {
            store: ChannelStore::new(mur_home),
            index: ChannelIndex::open(mur_home)?,
        })
    }

    /// Create a fresh channel whose participants are the local human (owner)
    /// and one agent (delegate). Used by both CLI and Hub when opening a chat.
    pub fn create_for_agent(&self, agent: &str) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: format!("chat with {agent}"),
            goal: Goal::default(),
            state: ChannelState::Working,
            owner: ChannelActor::local_human(),
            participants: vec![
                Participant { actor: ChannelActor::local_human(), role: ParticipantRole::Owner, joined_at: now },
                Participant { actor: ChannelActor::Agent { id: agent.to_string() }, role: ParticipantRole::Delegate, joined_at: now },
            ],
            created_at: now,
            updated_at: now,
        };
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        Ok(ch)
    }

    /// Append a message event and bump the manifest's `updated_at` + index.
    pub fn append_message(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        text: &str,
        task_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let mut payload = serde_json::json!({ "text": text });
        if let Some(t) = task_id {
            payload["task_id"] = serde_json::Value::String(t.to_string());
        }
        let ev = self.store.append_event(channel_id, actor, kind, payload, None)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(ev)
    }

    pub fn load_events(&self, channel_id: &str) -> Result<Vec<ChannelEvent>> {
        self.store.load_events(channel_id)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        self.index.list(limit)
    }

    /// The newest channel that has `agent` as a participant — the CLI's
    /// `--resume` target and the Hub's "open this agent" target.
    pub fn latest_for_agent(&self, agent: &str) -> Result<Option<String>> {
        // list() is newest-first; load each manifest and match the participant.
        for row in self.index.list(1000)? {
            if let Ok(ch) = self.store.load_manifest(&row.id)
                && ch.participants.iter().any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent))
            {
                return Ok(Some(ch.id));
            }
        }
        Ok(None)
    }

    pub fn store(&self) -> &ChannelStore {
        &self.store
    }
    pub fn index(&self) -> &ChannelIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_append_resume_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "find the bug", Some("t-1")).unwrap();
        svc.append_message(&ch.id, ChannelActor::Agent { id: "qa".into() }, EventKind::Message, "found it", Some("t-1")).unwrap();

        let evs = svc.load_events(&ch.id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "find the bug");

        let latest = svc.latest_for_agent("qa").unwrap();
        assert_eq!(latest.as_deref(), Some(ch.id.as_str()));
        assert!(svc.latest_for_agent("other").unwrap().is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-channel service`
Expected: FAIL until the impl compiles.

- [ ] **Step 3: (impl in Step 1)**

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p mur-channel service`
Expected: PASS — `create_append_resume_roundtrip`.

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/service.rs
git commit -m "feat(channel): ChannelService unifying store + index"
```

---

## Task 5: `watch` module (file-watch live sync)

**Files:**
- Create/replace: `mur-channel/src/watch.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test** — write `mur-channel/src/watch.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Watch `<mur_home>/channels/` recursively. For every filesystem event whose
/// path is inside a channel dir, invoke `on_change(channel_id)`. Returns the
/// watcher; the caller must keep it alive for the watch to persist.
pub fn watch_channels(
    mur_home: &Path,
    on_change: impl Fn(String) + Send + 'static,
) -> Result<RecommendedWatcher> {
    let root = mur_home.join("channels");
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    let root_clone = root.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        for path in event.paths {
            if let Ok(rel) = path.strip_prefix(&root_clone)
                && let Some(first) = rel.components().next()
                && let Some(id) = first.as_os_str().to_str()
            {
                on_change(id.to_string());
            }
        }
    })
    .context("create watcher")?;
    watcher.watch(&root, RecursiveMode::Recursive).context("start watch")?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    use crate::ChannelStore;
    use chrono::Utc;
    use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};

    #[test]
    fn append_fires_callback_with_channel_id() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let now = Utc::now();
        store
            .create(&Channel {
                v: 1, id: "c9".into(), title: "t".into(), goal: Goal::default(),
                state: ChannelState::Working, owner: ChannelActor::Human { name: "me".into() },
                participants: vec![], created_at: now, updated_at: now,
            })
            .unwrap();

        let (tx, rx) = mpsc::channel::<String>();
        let _watcher = watch_channels(tmp.path(), move |id| {
            let _ = tx.send(id);
        })
        .unwrap();

        // Give the watcher a moment to arm, then append.
        std::thread::sleep(Duration::from_millis(200));
        store
            .append_event("c9", ChannelActor::Human { name: "me".into() }, EventKind::Message, serde_json::json!({"text":"hi"}), None)
            .unwrap();

        let got = rx.recv_timeout(Duration::from_secs(5)).expect("callback fired");
        assert_eq!(got, "c9");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-channel watch`
Expected: FAIL until the impl compiles.

- [ ] **Step 3: (impl in Step 1)**

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p mur-channel watch`
Expected: PASS — `append_fires_callback_with_channel_id`. (If flaky on the CI `heavy` group, it is not vector-related; it uses a 5s timeout which is generous.)

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/watch.rs
git commit -m "feat(channel): notify-based channel file-watcher"
```

---

## Task 6: One-shot importer + `mur internals migrate-channels`

**Files:**
- Create: `mur-core/src/cmd/agent/channel_import.rs`
- Modify: `mur-core/Cargo.toml` (add `mur-channel`), `mur-core/src/cmd/agent/mod.rs` (declare module), `mur-core/src/dispatch.rs` (add `InternalsAction::MigrateChannels` arm), and the clap enum that defines `InternalsAction`.
- Test: `mur-core/tests/channel_import.rs`

- [ ] **Step 1: Add the dependency**

In `mur-core/Cargo.toml` `[dependencies]`, add:

```toml
mur-channel = { path = "../mur-channel" }
```

- [ ] **Step 2: Write the importer** — create `mur-core/src/cmd/agent/channel_import.rs`:

```rust
//! One-shot importer: turn legacy `~/.mur/agents/<agent>/cli-sessions/*.jsonl`
//! transcripts into Channels. Idempotent at the channel level via a marker file.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};

use super::cli::persist::{self, TurnRecord};

/// Import every CLI session of every agent under `mur_home`. Returns the number
/// of channels created. Sessions already imported (marker present) are skipped.
pub fn migrate_all(mur_home: &Path) -> Result<usize> {
    let svc = ChannelService::open(mur_home)?;
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(0);
    }
    let mut created = 0;
    for entry in fs::read_dir(&agents_dir).with_context(|| format!("read {}", agents_dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent = entry.file_name().to_string_lossy().to_string();
        for info in persist::list_recent(mur_home, &agent, usize::MAX)? {
            let marker = info.path.with_extension("imported");
            if marker.exists() {
                continue;
            }
            let turns = persist::load(&info.path)?;
            let ch = svc.create_for_agent(&agent)?;
            for t in &turns {
                let (actor, kind) = turn_actor(&agent, t);
                svc.append_message(&ch.id, actor, kind, &t.text, t.task_id.as_deref())?;
            }
            fs::write(&marker, ch.id.as_bytes()).ok();
            created += 1;
        }
    }
    Ok(created)
}

fn turn_actor(agent: &str, t: &TurnRecord) -> (ChannelActor, EventKind) {
    match t.role.as_str() {
        "agent" => (ChannelActor::Agent { id: agent.to_string() }, EventKind::Message),
        "shell" => (ChannelActor::System, EventKind::Note),
        _ => (ChannelActor::local_human(), EventKind::Message),
    }
}
```

Declare the module: in `mur-core/src/cmd/agent/mod.rs` add `pub mod channel_import;` (verified: siblings like `cli` are private `mod`; this one must be `pub` so `dispatch.rs` and integration tests can reach `crate::cmd::agent::channel_import`). `mur-core/src/cmd/mod.rs` already has `pub mod agent;` and lib.rs:23 has `pub mod cmd;`.

Ensure `persist`'s items are reachable: `TurnRecord`, `Session`, `SessionInfo`, `list_recent`, `load`, `latest` are already `pub` in the current `persist.rs`; the Task 7 rewrite keeps them `pub`. Module visibility (`pub mod cli;` / `pub mod persist;`) is changed in Task 7.

- [ ] **Step 3: Wire the command**

Add a variant to the `InternalsAction` clap enum in `mur-core/src/cli/actions.rs:140-165` (verified current variants: `Reindex`, `RebuildIndex`, `Git`):

```rust
/// One-shot: import legacy cli-sessions into Channels.
MigrateChannels,
```

In `mur-core/src/dispatch.rs`, add an arm next to the existing `InternalsAction::Reindex { .. }` arm:

```rust
InternalsAction::MigrateChannels => {
    let home = crate::paths::mur_root(None);
    let n = cmd::agent::channel_import::migrate_all(&home)?;
    println!("✅ imported {n} CLI session(s) into channels");
}
```

(If `cmd::agent::channel_import` is not visible from `dispatch.rs`, confirm `pub mod agent;` and `pub mod channel_import;` expose the path `crate::cmd::agent::channel_import`.)

- [ ] **Step 4: Write the failing integration test** — create `mur-core/tests/channel_import.rs`:

```rust
use std::fs;
use tempfile::TempDir;

#[test]
fn imports_cli_session_into_channel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    // Seed a legacy cli-session transcript.
    let sess_dir = home.join("agents/qa/cli-sessions");
    fs::create_dir_all(&sess_dir).unwrap();
    let jsonl = "\
{\"ts\":\"2026-06-15T10:00:00+00:00\",\"role\":\"user\",\"text\":\"hi\"}\n\
{\"ts\":\"2026-06-15T10:00:01+00:00\",\"role\":\"agent\",\"text\":\"hello\",\"task_id\":\"t-1\"}\n";
    fs::write(sess_dir.join("018f00000000-aaaa.jsonl"), jsonl).unwrap();

    let n = mur_core::cmd::agent::channel_import::migrate_all(home).unwrap();
    assert_eq!(n, 1);

    // The channel store now has one channel with two events.
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let rows = svc.list(10).unwrap();
    assert_eq!(rows.len(), 1);
    let evs = svc.load_events(&rows[0].id).unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[1].payload["text"], "hello");

    // Re-running is idempotent (marker file).
    let n2 = mur_core::cmd::agent::channel_import::migrate_all(home).unwrap();
    assert_eq!(n2, 0);
}
```

This test calls library functions directly (in-process), so it needs `mur_core::cmd::agent::channel_import` and `mur_channel` to be accessible from an integration test. Confirm `mur-core/src/lib.rs` re-exports `pub mod cmd;` (it does — `dispatch` calls `cmd::...`). Add `mur-channel` to `mur-core/Cargo.toml` `[dev-dependencies]` as well if the integration test cannot resolve it via the normal dep (it can, since it is a normal dependency):

(no extra dev-dep needed — `mur-channel` is a normal dependency.)

- [ ] **Step 5: Run the test to verify it fails, then passes**

Run: `cargo nextest run -p mur-core channel_import`
Expected: FAIL first (module not wired), then PASS — `imports_cli_session_into_channel`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/cmd/agent/channel_import.rs mur-core/src/cmd/agent/mod.rs mur-core/src/dispatch.rs mur-core/tests/channel_import.rs
git commit -m "feat(channel): one-shot cli-sessions importer + mur internals migrate-channels"
```

---

## Task 7: Redirect `mur agent cli` writes to the Channel store

**Files:**
- Modify (full rewrite): `mur-core/src/cmd/agent/cli/persist.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs:133-166` (`build_app`)
- Test: `mur-core/tests/cli_channel_persist.rs`

The `App` keeps calling `session.append(role, text, task_id)`, `Session::create(home, agent)`, `persist::latest`, `persist::load`, `App::load_history(Vec<TurnRecord>)`. We keep those signatures and swap the internals to the Channel store. `TurnRecord` stays as the in-memory shape.

- [ ] **Step 1: Rewrite `persist.rs`** — replace the file body with:

```rust
//! CLI transcript persistence, backed by the unified Channel store.
//! The public surface (`Session`, `TurnRecord`, `SessionInfo`, `list_recent`,
//! `load`, `latest`) is preserved so `app.rs`/`mod.rs` are barely touched.
use std::path::Path;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use serde::{Deserialize, Serialize};

/// In-memory shape consumed by `App::load_history`. (No longer a JSONL line.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub ts: String,
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Listing entry; `id` is the channel id (was the session file stem).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub preview: String,
    pub turns: usize,
}

/// A live conversation handle bound to one channel.
pub struct Session {
    svc: ChannelService,
    channel_id: String,
    agent: String,
}

impl Session {
    /// Open a fresh channel for `agent`.
    pub fn create(home: &Path, agent: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        let ch = svc.create_for_agent(agent)?;
        Ok(Self { svc, channel_id: ch.id, agent: agent.to_string() })
    }

    /// Re-open an existing channel by id (used by `--resume`).
    pub fn open_existing(home: &Path, agent: &str, channel_id: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        Ok(Self { svc, channel_id: channel_id.to_string(), agent: agent.to_string() })
    }

    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// Append one turn. `role` ∈ {"user","agent","shell"}.
    pub fn append(&self, role: &str, text: &str, task_id: Option<&str>) -> Result<()> {
        let (actor, kind) = match role {
            "agent" => (ChannelActor::Agent { id: self.agent.clone() }, EventKind::Message),
            "shell" => (ChannelActor::System, EventKind::Note),
            _ => (ChannelActor::local_human(), EventKind::Message),
        };
        self.svc.append_message(&self.channel_id, actor, kind, text, task_id)?;
        Ok(())
    }
}

fn event_to_turn(ev: &ChannelEvent, agent: &str) -> TurnRecord {
    let text = ev.payload.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let task_id = ev.payload.get("task_id").and_then(|v| v.as_str()).map(str::to_string);
    let role = match &ev.actor {
        ChannelActor::Agent { .. } => "agent",
        ChannelActor::System => "shell",
        ChannelActor::Human { .. } => "user",
    }
    .to_string();
    let _ = agent;
    TurnRecord { ts: ev.ts.to_rfc3339(), role, text, task_id }
}

/// Load a channel's turns for `App::load_history`.
pub fn load(home: &Path, channel_id: &str, agent: &str) -> Result<Vec<TurnRecord>> {
    let svc = ChannelService::open(home)?;
    Ok(svc.load_events(channel_id)?.iter().map(|e| event_to_turn(e, agent)).collect())
}

/// Newest channels that involve `agent`, newest-first.
pub fn list_recent(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    let svc = ChannelService::open(home)?;
    let mut out = Vec::new();
    for row in svc.list(1000)? {
        let evs = svc.load_events(&row.id)?;
        let involved = svc
            .store()
            .load_manifest(&row.id)
            .map(|ch| {
                ch.participants
                    .iter()
                    .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent))
            })
            .unwrap_or(false);
        if !involved {
            continue;
        }
        let preview = evs
            .iter()
            .find(|e| matches!(e.actor, ChannelActor::Human { .. }))
            .and_then(|e| e.payload.get("text").and_then(|v| v.as_str()))
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        out.push(SessionInfo { id: row.id, preview, turns: evs.len() });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

pub fn latest(home: &Path, agent: &str) -> Result<Option<SessionInfo>> {
    Ok(list_recent(home, agent, 1)?.into_iter().next())
}
```

- [ ] **Step 2: Update `build_app` in `mod.rs`** — replace lines 133-166 (`fn build_app`) with:

```rust
fn build_app(home: &Path, agent: &str, resume: bool) -> Result<App> {
    if resume {
        if let Some(info) = persist::latest(home, agent)? {
            let turns = persist::load(home, &info.id, agent)?;
            let mut app = App::new(
                home.to_path_buf(),
                agent.to_string(),
                Session::open_existing(home, agent, &info.id)?,
            );
            app.load_history(turns);
            app.push_system(format!(
                "resumed conversation ({} turns) — {HELP}",
                app.messages.len()
            ));
            return Ok(app);
        }
        let mut app = App::new(
            home.to_path_buf(),
            agent.to_string(),
            Session::create(home, agent)?,
        );
        app.push_system(format!("no saved conversation to resume; starting fresh. {HELP}"));
        return Ok(app);
    }
    let mut app = App::new(
        home.to_path_buf(),
        agent.to_string(),
        Session::create(home, agent)?,
    );
    app.push_system(HELP);
    Ok(app)
}
```

Note: this removes the old `Session::from_path(info.path)` and `persist::load(&info.path)` calls. If any other site references `SessionInfo.path` or `Session::from_path`, update it — grep first:

```bash
rg "from_path|info\.path|SessionInfo" mur-core/src/cmd/agent/cli/
```

Fix each hit to use `info.id` / `Session::open_existing`.

- [ ] **Step 3: Write the failing test** — create `mur-core/tests/cli_channel_persist.rs`:

```rust
use mur_core::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

#[test]
fn cli_turns_persist_to_channel_and_resume() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let sess = Session::create(home, "qa").unwrap();
    sess.append("user", "find the bug", None).unwrap();
    sess.append("agent", "found it", Some("t-1")).unwrap();
    let cid = sess.channel_id().to_string();
    drop(sess);

    // Resume picks the latest channel for the agent.
    let latest = persist::latest(home, "qa").unwrap().expect("a session");
    assert_eq!(latest.id, cid);
    let turns = persist::load(home, &latest.id, "qa").unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "agent");
    assert_eq!(turns[1].text, "found it");
}
```

Make the module path public (verified: both are currently private). Change `mod cli;` → `pub mod cli;` in `mur-core/src/cmd/agent/mod.rs`, and `mod persist;` → `pub mod persist;` in `mur-core/src/cmd/agent/cli/mod.rs`. `pub mod cmd;` (lib.rs:23) and `pub mod agent;` (cmd/mod.rs) already hold, so `mur_core::cmd::agent::cli::persist` then resolves from `mur-core/tests/`; the other `cli` children stay private.

- [ ] **Step 4: Run the test to verify it fails, then passes**

Run: `cargo nextest run -p mur-core cli_channel_persist`
Expected: FAIL first (signature mismatch / module privacy), then PASS after Steps 1-2.

- [ ] **Step 5: Build the whole workspace to catch call-site drift**

Run: `cargo build -p mur-core`
Expected: clean build. The rewrite removes `Session::from_path`, `Session::path()`, the `SessionInfo.path` field, and the `SESSIONS_SUBDIR`/`SESSION_EXT`/`RECENT_LIMIT` consts. Fix every site the compiler flags — verified candidates: `cli/mod.rs:136` (`persist::load(&info.path)`) and `cli/mod.rs:140` (`Session::from_path(info.path)`), both handled in Step 2; and check `cli/manage.rs` (the `mur agent cli` session-list path may call `list_recent` / read `SessionInfo.path`) — update it to use `info.id` / `info.preview` / `info.turns` only.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/persist.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/tests/cli_channel_persist.rs
git commit -m "feat(channel): back mur agent cli transcripts with the Channel store"
```

---

## Task 8: Hub backend — persist chat to channels + `channel_load` + watcher

**Files:**
- Modify: `mur-hub-gui/src-tauri/Cargo.toml` (add `mur-channel`, `mur-common` if absent)
- Modify: `mur-hub-gui/src-tauri/src/chat.rs` (persist turns; add `channel_load`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register `channel_load`; spawn watcher)
- Test: a Rust unit test for the persistence helper + manual verification steps

- [ ] **Step 1: Add dependencies**

In `mur-hub-gui/src-tauri/Cargo.toml` `[dependencies]` add ONE line (verified: `mur-common`, `mur-core`, `mur-gui-core` are already declared here as `{ path = "../../<crate>" }`, and the crate is Tauri 2):

```toml
mur-channel = { path = "../../mur-channel" }
```

- [ ] **Step 2: Add a persistence helper + `channel_load` command** — append to `mur-hub-gui/src-tauri/src/chat.rs`:

```rust
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// Resolve the channel for an agent (latest, or create one), returning its id.
fn channel_for_agent(home: &std::path::Path, agent: &str) -> anyhow::Result<String> {
    let svc = ChannelService::open(home)?;
    if let Some(id) = svc.latest_for_agent(agent)? {
        return Ok(id);
    }
    Ok(svc.create_for_agent(agent)?.id)
}

/// Persist one turn into the agent's channel. `role` ∈ {"user","agent"}.
fn persist_turn(home: &std::path::Path, agent: &str, role: &str, text: &str, task_id: Option<&str>) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = channel_for_agent(home, agent)?;
        let actor = match role {
            "agent" => ChannelActor::Agent { id: agent.to_string() },
            _ => ChannelActor::local_human(),
        };
        svc.append_message(&id, actor, EventKind::Message, text, task_id)?;
        Ok(())
    })();
    if let Err(e) = res {
        eprintln!("channel persist failed for {agent}: {e:#}");
    }
}

/// Tauri command: load the agent's latest channel events for hydration.
#[tauri::command]
pub async fn channel_load(name: String) -> Result<Vec<ChannelEvent>, String> {
    let home = crate::mur_home_path();
    let svc = ChannelService::open(&home).map_err(|e| e.to_string())?;
    let Some(id) = svc.latest_for_agent(&name).map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    svc.load_events(&id).map_err(|e| e.to_string())
}
```

Then, inside `agent_chat_send` (chat.rs lines 72-169), persist the user turn and the final reply. After the params are built (around line 93) add:

```rust
let home = mur_core::paths::mur_root(None);
persist_turn(&home, &name, "user", &text, Some(&task_id));
```

And just before the final return — which is exactly `Ok(ChatReply { reply, task_id, streamed })` (verified, chat.rs ~line 168) — insert (uses `&reply`/`&task_id` before they are moved into `ChatReply`; at the tail `task_id` has been rebound to the response id, i.e. the agent turn's id):

```rust
persist_turn(&home, &name, "agent", &reply, Some(&task_id));
```

Home resolution: the crate exposes `pub(crate) fn mur_home_path()` (lib.rs ~514, the same helper used for dialing) — call `crate::mur_home_path()` (as in the code above). The earlier user-turn `persist_turn` runs where `task_id` is still the incoming request id (the command param), which is correct for the user turn.

- [ ] **Step 3: Register the command + spawn the watcher** — in `mur-hub-gui/src-tauri/src/lib.rs`:

Add `chat::channel_load,` to the `tauri::generate_handler!` list (the block at lines 420-480, next to `chat::agent_chat_send`).

In the app setup (the `.setup(|app| { ... })` closure, or right after `.manage(...)` calls near line 243), spawn the watcher and emit `channel-updated`:

```rust
{
    use tauri::Manager;
    let handle = app.handle().clone();
    let home = crate::mur_home_path();
    // Keep the watcher alive for the app's lifetime by leaking it into managed state.
    match mur_channel::watch::watch_channels(&home, move |channel_id| {
        let _ = handle.emit("channel-updated", channel_id);
    }) {
        Ok(watcher) => {
            app.manage(std::sync::Mutex::new(Some(watcher)));
        }
        Err(e) => eprintln!("channel watcher failed to start: {e:#}"),
    }
}
```

(If `app.handle().emit` is not available in this Tauri version, use `app.emit_all`/`handle.emit_to` as used elsewhere in this file — match the existing emit call style, e.g. how `agents-updated` is emitted.)

- [ ] **Step 4: Unit-test the persistence helper** — add to `chat.rs` `#[cfg(test)]`:

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn persist_turn_writes_channel_event() {
        let tmp = TempDir::new().unwrap();
        persist_turn(tmp.path(), "qa", "user", "hello hub", Some("t-1"));
        let svc = ChannelService::open(tmp.path()).unwrap();
        let id = svc.latest_for_agent("qa").unwrap().expect("channel");
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].payload["text"], "hello hub");
    }
}
```

Add `tempfile = "3"` to `mur-hub-gui/src-tauri/Cargo.toml` `[dev-dependencies]` if absent.

- [ ] **Step 5: Run the unit test**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml channel_tests`
Expected: PASS — `persist_turn_writes_channel_event`. (The Hub crate is workspace-excluded, so use `--manifest-path`, not `-p`.)

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/Cargo.toml mur-hub-gui/src-tauri/src/chat.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(channel): Hub persists chat to channel store + channel_load + watcher"
```

---

## Task 9: Hub frontend — hydrate from channel on mount + on `channel-updated`

**Files:**
- Modify: `mur-hub-gui/ui/src/components/ChatTab.tsx`
- Test: manual (Tauri UI)

- [ ] **Step 1: Add a type + load helper** — near the other interfaces in `ChatTab.tsx`, add:

```typescript
interface ChannelEvent {
  seq: number;
  ts: string;
  actor: { kind: "human" | "agent" | "system"; name?: string; id?: string };
  kind: string;
  payload: { text?: string; task_id?: string };
}

function eventToMessage(ev: ChannelEvent) {
  const role = ev.actor.kind === "agent" ? "agent" : ev.actor.kind === "system" ? "system" : "user";
  return { role, text: ev.payload.text ?? "" };
}
```

- [ ] **Step 2: Hydrate on mount and on live updates** — extend the existing `useEffect([agentName])` (the explore notes line 78). Add a loader that runs on mount and whenever a `channel-updated` event names a channel for this agent:

```typescript
useEffect(() => {
  let alive = true;
  const hydrate = async () => {
    try {
      const events = await invoke<ChannelEvent[]>("channel_load", { name: agentName });
      if (!alive) return;
      setMessages(events.map(eventToMessage)); // replace with the component's message setter
    } catch (e) {
      console.error("channel_load failed", e);
    }
  };
  hydrate();
  const un = listen<string>("channel-updated", () => {
    // Re-hydrate on any channel change; cheap at single-user scale.
    hydrate();
  });
  return () => {
    alive = false;
    un.then((f) => f());
  };
}, [agentName]);
```

Adapt `setMessages` to the component's actual message state setter (the explore shows messages render from a `messages` accumulator). Ensure live streaming deltas (the existing `chat-delta` path) still append on top of the hydrated history — hydration sets the committed history; streaming appends the in-flight reply, which is then persisted by the backend and shows up on the next `channel-updated` re-hydrate.

- [ ] **Step 3: Manual verification**

```text
1. Build the Hub: ./build.sh   (or the Hub's own build per CLAUDE.md)
2. Run `mur internals migrate-channels` once to import existing CLI history.
3. Open the Hub, chat with agent "qa", send a message, get a reply.
4. Close and reopen the Hub → the conversation is still there (was in-memory before).
5. In a terminal run `mur agent cli qa`, send a message.
6. Watch the Hub's qa chat update live (channel-updated → re-hydrate).
```

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/ChatTab.tsx
git commit -m "feat(channel): Hub ChatTab hydrates from shared channel store live"
```

---

## Task 10: `reindex` rebuilds the SQLite index + cross-surface integration test + docs

**Files:**
- Modify: `mur-core/src/cmd/reindex.rs` (also rebuild the channel index)
- Test: `mur-core/tests/channel_cross_surface.rs`
- Modify: `docs/architecture/runtime-overview.md` (document `~/.mur/channels/` + the index)

- [ ] **Step 1: Extend reindex** — in `mur-core/src/cmd/reindex.rs`, at the end of `cmd_reindex()` (before `Ok(())`), add a channel-index rebuild so a deleted `channels.db` is recoverable:

```rust
// Rebuild the Channel SQLite read-model from the event-log manifests.
{
    let home = crate::paths::mur_root(None);
    let store = mur_channel::ChannelStore::new(&home);
    let index = mur_channel::index::ChannelIndex::open(&home)?;
    let n = index.rebuild_from(&store)?;
    println!("✅ rebuilt channel index: {n} channel(s)");
}
```

(Export `ChannelIndex` from `mur-channel` — confirm `pub mod index;` and either `pub use index::ChannelIndex;` in `mur-channel/src/lib.rs` or reference the full path `mur_channel::index::ChannelIndex` as above.)

- [ ] **Step 2: Write the failing cross-surface integration test** — create `mur-core/tests/channel_cross_surface.rs`:

```rust
use mur_channel::ChannelService;
use mur_core::cmd::agent::cli::persist::Session;
use tempfile::TempDir;

// Simulates: CLI appends a turn; the "Hub side" (a second ChannelService on the
// same home) reads the very same event. This is the v1 payoff — one store, two
// surfaces.
#[test]
fn cli_write_is_visible_to_a_second_reader() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // CLI side writes.
    let sess = Session::create(home, "qa").unwrap();
    sess.append("user", "shared message", None).unwrap();
    let cid = sess.channel_id().to_string();

    // Hub side reads the same on-disk store.
    let hub = ChannelService::open(home).unwrap();
    let evs = hub.load_events(&cid).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].payload["text"], "shared message");

    // And the index lists it for the agent.
    assert_eq!(hub.latest_for_agent("qa").unwrap().as_deref(), Some(cid.as_str()));
}

// Deleting the SQLite index and rebuilding from logs restores the listing.
#[test]
fn index_is_rebuildable_from_logs() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let sess = Session::create(home, "qa").unwrap();
    sess.append("user", "x", None).unwrap();

    // Nuke the index DB.
    std::fs::remove_file(home.join("index/channels.db")).unwrap();

    // Rebuild from the store and confirm the listing returns.
    let store = mur_channel::ChannelStore::new(home);
    let index = mur_channel::index::ChannelIndex::open(home).unwrap();
    let n = index.rebuild_from(&store).unwrap();
    assert_eq!(n, 1);
    assert_eq!(index.list(10).unwrap().len(), 1);
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo nextest run -p mur-core channel_cross_surface`
Expected: PASS — `cli_write_is_visible_to_a_second_reader`, `index_is_rebuildable_from_logs`.

- [ ] **Step 4: Run the full workspace suite (catch regressions)**

Run: `cargo nextest run --workspace`
Expected: PASS (note: per project memory, plain `cargo test --workspace` is flaky — use nextest). Investigate any new failures introduced by the persist.rs rewrite.

- [ ] **Step 5: Lint**

Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean. Fix warnings (e.g. unused `agent` param in `event_to_turn` — already handled with `let _ = agent;`, or remove the param if clippy prefers).

- [ ] **Step 6: Document** — add a short subsection to `docs/architecture/runtime-overview.md` describing the new data location:

```markdown
### Channels (unified work object)

`~/.mur/channels/<id>/` holds one Channel each:
- `events.jsonl` — append-only event stream (source of truth).
- `channel.yaml` — manifest cache (goal/state/participants), recomputable from the log.

`~/.mur/index/channels.db` is a rebuildable SQLite read-model (channel list / "my
work" inbox). Rebuild with `mur internals reindex`. Import legacy CLI transcripts
with `mur internals migrate-channels`. Spec: `docs/superpowers/specs/2026-06-15-unified-channel-design.md`.
```

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/reindex.rs mur-core/tests/channel_cross_surface.rs docs/architecture/runtime-overview.md
git commit -m "feat(channel): reindex rebuilds channel index + cross-surface tests + docs"
```

---

## Self-Review (completed by plan author)

**Spec coverage:**
- D1 unified primitive → Task 1 (`Channel` with goal/state/participants/events). ✅
- D2 foundation-first → entire plan is the foundation; no UI/orchestration. ✅
- D3 Hub + CLI (iOS later) → Tasks 7 (CLI), 8-9 (Hub); iOS untouched. ✅
- D4 event-sourced JSONL + file-watch (daemon optional) → Tasks 2 (log), 5 (watch). No daemon dependency. ✅
- D5 log-as-truth + SQLite index (Postgres server-only) → Tasks 3, 10 (rebuildable). ✅
- D9 canonical store + one-shot importer, no dual-write → Tasks 6 (importer), 7 (CLI writes only to channels). ✅
- D10 distinct `ChannelState` mapped at boundary → Task 1 (own enum; no `a2a::TaskState` churn). ✅
- Cross-surface "same chat" payoff → Task 10 integration test. ✅
- LanceDB semantic index → explicitly deferred per the Scope note (reuses existing reindex pipeline). ✅ (documented deferral, not a silent gap)

**Placeholder scan:** no TBD/TODO; every code step shows complete code; every run step shows the command + expected result. ✅

**Type consistency:** `ChannelActor` (not `Actor`) used everywhere; `ChannelService::{open, create_for_agent, append_message, load_events, list, latest_for_agent, store, index}` names match across Tasks 4/6/7/8/10; `ChannelStore::{new, create, save_manifest, load_manifest, load_events, append_event, list_ids}` consistent; `ChannelIndex::{open, upsert, list, rebuild_from}` consistent; `persist::{Session, TurnRecord, SessionInfo, list_recent, load, latest}` preserved and call sites updated (Task 7 Step 2). ✅

**Known integration risks flagged inline (verify during execution):** exact relative dep path depth for `mur-hub-gui/src-tauri` (Task 8 Step 1); the precise reply-text/task-id identifiers in `agent_chat_send` (Task 8 Step 2); the Tauri `emit` API spelling for this version (Task 8 Step 3); module `pub` visibility for `mur_core::cmd::agent::cli::persist` and `channel_import` (Tasks 6/7).
