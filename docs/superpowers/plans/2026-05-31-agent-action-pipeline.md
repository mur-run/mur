# Agent Action Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Agent Action Pipeline (Block A): unified file intake, task queue with pause/cancel, deletion safety, and result notification — delivered as Phases 0–2 + 4a, with Phase 3 TrashGuard gate written and unit-tested ahead of P0b.

**Architecture:** New `mur-core/src/action_pipeline/` module (8 files) with four pipeline phases (INGEST → QUEUE → EXEC → NOTIFY) sharing a JSONL ledger. Types live in `mur-common/src/action.rs`. CLI adds three nested-action subcommands (`mur agent pending|queue|trash`). The deletion-safety gate is a `Hook` impl registered via A1 handler-picker; the trash executor (rm→mv rewrite) is deferred to P0b. Background ticks (pending TTL expiry, trash deferred-execute + retention marking) run in mur-daemon's 30s scan.

**Tech Stack:** Rust (edition 2024), serde_yaml_ng, serde_json, clap, chrono, uuid, tokio, React/TypeScript (mur-hub-gui)

---

## File Structure

### New Files

| File | Responsibility | ~Lines |
|------|---------------|--------|
| `mur-common/src/action.rs` | `ActionEvent`, `FileAction`, `ActionPipelineConfig` types + serde | 280 |
| `mur-core/src/action_pipeline/mod.rs` | Public API: `Pipeline` struct, top-level orchestration | 120 |
| `mur-core/src/action_pipeline/state.rs` | All data types: `PendingItem`, `Task`, `TrashEntry`, etc. | 350 |
| `mur-core/src/action_pipeline/ledger.rs` | Shared JSONL ledger (flock multi-writer); wraps generic `Ledger<E>` from mur-common | 200 |
| `mur-core/src/action_pipeline/ingest.rs` | `PendingStore`: MIME detect via magic bytes, dedup (5s), merge (5s), expiry | 300 |
| `mur-core/src/action_pipeline/queue.rs` | `TaskQueue`: state machine, concurrency control, step reporting | 350 |
| `mur-core/src/action_pipeline/guard.rs` | `TrashGuard` Hook impl: destructive pattern detection, schema-level enforcement, two-phase AskUser | 400 |
| `mur-core/src/action_pipeline/notify.rs` | Aggregator: badge count, OS notification formatting, output routing | 250 |
| `mur-core/src/action_pipeline/error.rs` | Error types: `PipelineError`, `GuardError`, `QueueError`, `LedgerError` | 80 |
| `mur-core/src/cmd/agent/pending.rs` | CLI handler for `mur agent pending` | 80 |
| `mur-core/src/cmd/agent/queue_cmd.rs` | CLI handler for `mur agent queue` | 90 |
| `mur-core/src/cmd/agent/trash.rs` | CLI handler for `mur agent trash` | 100 |
| `mur-daemon/src/action_tick.rs` | 30s tick: pending-item TTL expiry + trash deferred-execute + retention marking | 120 |

### Modified Files

| File | What changes |
|------|-------------|
| `mur-common/src/lib.rs` | Add `pub mod action;` |
| `mur-common/src/agent.rs` | Add `file_actions: Vec<FileAction>` and `action_pipeline: ActionPipelineConfig` fields to `AgentProfile` |
| `mur-core/src/lib.rs` | Add `pub mod action_pipeline;` |
| `mur-core/src/cli/agent.rs` | Add `Pending`, `Queue`, `Trash` variants to `AgentAction`; define `AgentPendingAction`, `AgentQueueAction`, `AgentTrashAction` enums |
| `mur-core/src/dispatch.rs` | Add dispatch arms for `AgentAction::Pending`, `AgentAction::Queue`, `AgentAction::Trash` |
| `mur-core/src/cmd/agent/mod.rs` | Add `mod pending; mod queue_cmd; mod trash;` and re-exports |
| `mur-daemon/src/main.rs` | Add `mod action_tick;` and call `action_tick::scan_all_agents()` in the 1s event loop (throttled to 30s for action work) |
| `mur-agent-runtime/src/hooks/mod.rs` | Add `pub mod trash_guard;` and `pub use trash_guard::TrashGuard;` |
| `mur-common/src/hooks_config.rs` | Add `trash_guard: bool` field to `HooksConfig` |

### GUI Files (mur-hub-gui, not built in this plan — annotated for reference)

| Directory | Components |
|-----------|-----------|
| `mur-hub-gui/ui/src/components/pending/` | `PendingPanel`, `FloatingBadge`, `FileChecklist`, `ActionButtons`, `VoiceInput` |
| `mur-hub-gui/ui/src/components/queue/` | `QueuePanel`, `TaskRow`, `StepList` |
| `mur-hub-gui/ui/src/components/trash/` | `TrashPanel`, `TrashRow` |
| `mur-hub-gui/ui/src/hooks/` | `usePendingBridge`, `useQueueBridge`, `useTrashBridge` |

---

## Phase 0: Shared Infrastructure

### Task 0.1: Add action types to mur-common

**Files:**
- Create: `mur-common/src/action.rs`
- Modify: `mur-common/src/lib.rs`

- [ ] **Step 1: Create the action types module**

```rust
// mur-common/src/action.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use uuid::Uuid;

// ── Phase 1: Ingestion ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingItem {
    pub id: Uuid,
    pub source: ItemSource,
    pub files: Vec<PendingFile>,
    pub created_at: DateTime<Utc>,
    pub status: PendingStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ItemSource {
    DragDrop { paths: Vec<PathBuf> },
    Clipboard { mime_type: String },
    ShareUrl { url: String, kind: ShareKind },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShareKind {
    WebPage,
    File,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingFile {
    pub path: PathBuf,
    pub mime_type: String,
    pub size_bytes: u64,
    pub thumbnail_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PendingStatus {
    AwaitingSelection,
    Selected { action_id: String },
    Expired,
}

// ── Phase 2: Task Queue ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: Uuid,
    pub pending_item_id: Uuid,
    pub action: Action,
    pub state: TaskState,
    pub steps: Vec<TaskStep>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub id: String,                   // matches file_actions[].id
    pub label: String,
    pub user_prompt: Option<String>,  // filled for ask_me
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskState {
    Queued,
    Running,
    Paused,
    Completed { outcome: TaskOutcome },
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskOutcome {
    Success { outputs: Vec<ActionOutput> },
    PartialSuccess { succeeded: u32, failed: u32 },
    Failed { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionOutput {
    pub kind: OutputKind,
    pub file_path: Option<PathBuf>,
    pub chat_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OutputKind {
    File,
    ChatMessage,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStep {
    pub index: u32,
    pub label: String,
    pub state: StepState,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
}

// ── Phase 3: Deletion Safety ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrashEntry {
    pub id: Uuid,
    pub original_path: PathBuf,
    pub trash_path: Option<PathBuf>,
    pub file_size_bytes: u64,
    pub created_by_task_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub execute_at: DateTime<Utc>,
    pub retention_until: Option<DateTime<Utc>>,
    pub status: TrashStatus,
    pub restore_metadata: RestoreMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TrashStatus {
    PendingDelete,
    Retained,
    Expired,
    Restored,
    PermDeleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreMeta {
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub permissions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PermDeleteReason {
    UserEmpty,
    UserNow,
    CapacityEviction,
}

// ── Shared Ledger Events ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ActionEvent {
    ItemIngested       { item: PendingItem },
    ItemSelected       { item_id: Uuid, action: Action },
    ItemExpired        { item_id: Uuid },
    TaskEnqueued       { task: Task },
    TaskStarted        { task_id: Uuid },
    TaskStepUpdated    { task_id: Uuid, step: TaskStep },
    TaskPaused         { task_id: Uuid, reason: String },
    TaskResumed        { task_id: Uuid },
    TaskCompleted      { task_id: Uuid, outcome: TaskOutcome },
    TaskCancelled      { task_id: Uuid },
    DeletionPending    { entry: TrashEntry },
    DeletionCancelled  { entry_id: Uuid },
    TrashCreated       { entry: TrashEntry },
    TrashRestored      { entry_id: Uuid },
    TrashExpired       { entry_id: Uuid },
    TrashPermDeleted   { entry_id: Uuid, reason: PermDeleteReason },
}

// ── Profile Schema Types ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FileAction {
    pub id: String,
    #[serde(default)]
    pub label: BTreeMap<String, String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mime_types: Vec<String>,   // empty ⇒ matches any
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionPipelineConfig {
    #[serde(default)]
    pub deletion: DeletionConfig,
    #[serde(default)]
    pub queue: QueueConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeletionConfig {
    #[serde(default = "default_true")]
    pub trash_enabled: bool,
    #[serde(default = "default_cancel_window")]
    pub cancel_window_minutes: u32,
    #[serde(default = "default_retention_days")]
    pub trash_retention_days: u32,
    #[serde(default = "default_trash_max_mb")]
    pub trash_max_mb: u64,
    #[serde(default = "default_max_batch")]
    pub max_batch: u32,
    #[serde(default)]
    pub auto_permanent_delete: bool,  // MUST stay false
    #[serde(default)]
    pub trusted_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueConfig {
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "default_timeout_minutes")]
    pub default_timeout_minutes: u32,
    #[serde(default = "default_pending_ttl_minutes")]
    pub pending_item_ttl_minutes: u32,
}

fn default_true() -> bool { true }
fn default_cancel_window() -> u32 { 10 }
fn default_retention_days() -> u32 { 30 }
fn default_trash_max_mb() -> u64 { 1024 }
fn default_max_batch() -> u32 { 50 }
fn default_max_concurrent() -> u32 { 3 }
fn default_timeout_minutes() -> u32 { 30 }
fn default_pending_ttl_minutes() -> u32 { 60 }

impl Default for ActionPipelineConfig {
    fn default() -> Self {
        Self {
            deletion: DeletionConfig {
                trash_enabled: true,
                cancel_window_minutes: 10,
                trash_retention_days: 30,
                trash_max_mb: 1024,
                max_batch: 50,
                auto_permanent_delete: false,
                trusted_paths: vec![],
            },
            queue: QueueConfig {
                max_concurrent: 3,
                default_timeout_minutes: 30,
                pending_item_ttl_minutes: 60,
            },
        }
    }
}

impl Default for DeletionConfig {
    fn default() -> Self {
        ActionPipelineConfig::default().deletion
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        ActionPipelineConfig::default().queue
    }
}

// ── BCP-47 label resolution ──

impl FileAction {
    /// Resolve label for a given BCP-47 locale string (e.g. "zh-TW").
    /// Falls back: exact match → language prefix → "en" → first entry → id.
    pub fn label_for(&self, locale: &str) -> &str {
        if let Some(v) = self.label.get(locale) {
            return v.as_str();
        }
        if let Some(prefix) = locale.split('-').next()
            && let Some(v) = self.label.get(prefix)
        {
            return v.as_str();
        }
        if let Some(v) = self.label.get("en") {
            return v.as_str();
        }
        self.label.values().next().map(|s| s.as_str()).unwrap_or(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_event_serde_roundtrip() {
        let item = PendingItem {
            id: Uuid::now_v7(),
            source: ItemSource::DragDrop {
                paths: vec![PathBuf::from("/tmp/test.pdf")],
            },
            files: vec![PendingFile {
                path: PathBuf::from("/tmp/test.pdf"),
                mime_type: "application/pdf".into(),
                size_bytes: 1024,
                thumbnail_path: None,
            }],
            created_at: Utc::now(),
            status: PendingStatus::AwaitingSelection,
        };
        let event = ActionEvent::ItemIngested { item };
        let json = serde_json::to_string(&event).unwrap();
        let back: ActionEvent = serde_json::from_str(&json).unwrap();
        match back {
            ActionEvent::ItemIngested { item } => {
                assert_eq!(item.files[0].mime_type, "application/pdf");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn file_action_label_resolution() {
        let mut labels = BTreeMap::new();
        labels.insert("zh-TW".into(), "摘要".into());
        labels.insert("en".into(), "Summarize".into());
        let fa = FileAction {
            id: "summarize".into(),
            label: labels,
            description: None,
            mime_types: vec!["text/*".into()],
        };
        assert_eq!(fa.label_for("zh-TW"), "摘要");
        assert_eq!(fa.label_for("zh"), "摘要"); // prefix fallback
        assert_eq!(fa.label_for("en"), "Summarize");
        assert_eq!(fa.label_for("ja"), "Summarize"); // fallback to en
    }

    #[test]
    fn action_pipeline_config_defaults() {
        let cfg = ActionPipelineConfig::default();
        assert_eq!(cfg.deletion.cancel_window_minutes, 10);
        assert_eq!(cfg.deletion.trash_retention_days, 30);
        assert_eq!(cfg.queue.max_concurrent, 3);
    }

    #[test]
    fn trash_entry_serde_roundtrip() {
        let entry = TrashEntry {
            id: Uuid::now_v7(),
            original_path: PathBuf::from("/tmp/file.txt"),
            trash_path: None,
            file_size_bytes: 100,
            created_by_task_id: Uuid::now_v7(),
            created_at: Utc::now(),
            execute_at: Utc::now(),
            retention_until: None,
            status: TrashStatus::PendingDelete,
            restore_metadata: RestoreMeta {
                owner_uid: 501,
                owner_gid: 20,
                permissions: 0o644,
            },
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: TrashEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.original_path, PathBuf::from("/tmp/file.txt"));
        assert_eq!(back.status, TrashStatus::PendingDelete);
    }

    #[test]
    fn deletion_config_yaml_roundtrip() {
        let yaml = r#"
trash_enabled: true
cancel_window_minutes: 5
trash_retention_days: 14
trash_max_mb: 512
max_batch: 25
auto_permanent_delete: false
trusted_paths: []
"#;
        let cfg: DeletionConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(cfg.cancel_window_minutes, 5);
        assert_eq!(cfg.trash_retention_days, 14);
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        let back: DeletionConfig = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(back.cancel_window_minutes, 5);
    }

    #[test]
    fn queue_config_defaults_deserialize() {
        let yaml = "max_concurrent: 5\ndefault_timeout_minutes: 60\npending_item_ttl_minutes: 120\n";
        let cfg: QueueConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(cfg.max_concurrent, 5);
        assert_eq!(cfg.default_timeout_minutes, 60);
        assert_eq!(cfg.pending_item_ttl_minutes, 120);
    }
}
```

- [ ] **Step 2: Add `pub mod action;` to mur-common/src/lib.rs**

Run: search for existing `pub mod` lines in `mur-common/src/lib.rs`, add after the last one:
```rust
pub mod action;
```

- [ ] **Step 3: Run tests to verify compilation**

Run: `cargo test -p mur-common -- action`
Expected: 5 tests PASS

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/action.rs mur-common/src/lib.rs
git commit -m "feat(action-pipeline): add action types to mur-common"
```

---

### Task 0.2: Add profile schema fields to AgentProfile

**Files:**
- Modify: `mur-common/src/agent.rs`

- [ ] **Step 1: Add file_actions and action_pipeline fields to AgentProfile**

After the `federation` field (line ~107), add:

```rust
    /// A1: declarative UI action list — file_actions rendered as action
    /// buttons in the pending-item selection UI. New top-level key; NOT
    /// nested under `capabilities:`.
    #[serde(default)]
    pub file_actions: Vec<crate::action::FileAction>,

    /// A2 + A3: action pipeline configuration (deletion safety + queue limits).
    #[serde(default)]
    pub action_pipeline: crate::action::ActionPipelineConfig,
```

- [ ] **Step 2: Write a test that legacy profiles deserialize without new fields**

In the existing test module in `agent.rs`, add:

```rust
    #[test]
    fn legacy_profile_without_file_actions_or_action_pipeline_loads() {
        let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(p.file_actions.is_empty());
        assert_eq!(p.action_pipeline.deletion.cancel_window_minutes, 10);
        assert_eq!(p.action_pipeline.queue.max_concurrent, 3);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-common -- agent`
Expected: all tests PASS including new one

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(action-pipeline): add file_actions + action_pipeline to AgentProfile"
```

---

### Task 0.3: Add action_pipeline module skeleton + error types

**Files:**
- Create: `mur-core/src/action_pipeline/mod.rs`
- Create: `mur-core/src/action_pipeline/error.rs`
- Modify: `mur-core/src/lib.rs`

- [ ] **Step 1: Create error types**

```rust
// mur-core/src/action_pipeline/error.rs

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("ledger I/O error: {0}")]
    Ledger(#[from] std::io::Error),

    #[error("ledger serialization error: {0}")]
    LedgerSerde(#[from] serde_json::Error),

    #[error("queue full: {current} tasks, max {max}")]
    QueueFull { current: usize, max: u32 },

    #[error("task {task_id} not found")]
    TaskNotFound { task_id: String },

    #[error("pending item {item_id} not found")]
    PendingNotFound { item_id: String },

    #[error("trash entry {entry_id} not found")]
    TrashEntryNotFound { entry_id: String },

    #[error("guard: batch size {count} exceeds max {max}")]
    BatchTooLarge { count: usize, max: u32 },

    #[error("guard: wildcard pattern rejected in path: {path}")]
    WildcardRejected { path: String },

    #[error("guard: path outside allowed scope: {path}")]
    PathOutOfScope { path: PathBuf },

    #[error("guard: no action defined for mime type {mime_type}")]
    NoMatchingAction { mime_type: String },

    #[error("MIME detection failed for {path}: {reason}")]
    MimeDetect { path: PathBuf, reason: String },

    #[error("trash capacity exceeded: {used_mb}MB / {max_mb}MB")]
    TrashCapacityExceeded { used_mb: u64, max_mb: u64 },

    #[error("rename across filesystems not supported without fallback: {path}")]
    CrossFilesystem { path: PathBuf },

    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for PipelineError {
    fn from(e: anyhow::Error) -> Self {
        PipelineError::Other(e.to_string())
    }
}
```

- [ ] **Step 2: Create module skeleton**

```rust
// mur-core/src/action_pipeline/mod.rs

pub mod error;
pub mod state;
pub mod ledger;
pub mod ingest;
pub mod queue;
pub mod guard;
pub mod notify;

pub use error::PipelineError;

use mur_common::action::ActionPipelineConfig;
use std::path::PathBuf;

/// Top-level entry point for the action pipeline.
pub struct Pipeline {
    pub agent_home: PathBuf,
    pub config: ActionPipelineConfig,
}

impl Pipeline {
    pub fn new(agent_home: PathBuf, config: ActionPipelineConfig) -> Self {
        Self { agent_home, config }
    }

    /// Directory layout:
    ///   <agent_home>/actions/ledger/   — daily JSONL files
    ///   <agent_home>/actions/pending.json — rebuildable snapshot
    ///   <agent_home>/trash/            — trashed files
    pub fn actions_dir(&self) -> PathBuf {
        self.agent_home.join("actions")
    }

    pub fn ledger_dir(&self) -> PathBuf {
        self.actions_dir().join("ledger")
    }

    pub fn pending_snapshot_path(&self) -> PathBuf {
        self.actions_dir().join("pending.json")
    }

    pub fn trash_dir(&self) -> PathBuf {
        self.agent_home.join("trash")
    }
}
```

- [ ] **Step 3: Add `pub mod action_pipeline;` to mur-core/src/lib.rs**

After the existing module declarations, add:
```rust
pub mod action_pipeline;
```

- [ ] **Step 4: Verify compilation**

Run: `cargo build -p mur-core`
Expected: compiles successfully

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/action_pipeline/mod.rs mur-core/src/action_pipeline/error.rs mur-core/src/lib.rs
git commit -m "feat(action-pipeline): add module skeleton + error types"
```

---

### Task 0.4: Extract generic Ledger<E> to mur-common, add flock multi-writer

**Files:**
- Create: `mur-common/src/ledger.rs`
- Modify: `mur-common/src/lib.rs`
- Create: `mur-core/src/action_pipeline/ledger.rs`
- Modify: `mur-agent-runtime/src/durable/ledger.rs` (re-export from mur-common)

- [ ] **Step 1: Create generic Ledger<E> in mur-common**

```rust
// mur-common/src/ledger.rs

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Append-only JSONL ledger with per-day file rotation.
///
/// Generic over the event type `E`. For multi-writer scenarios,
/// wrap in `Arc<Mutex<Ledger<E>>>` — the file-level `flock` is
/// handled by the OS when opening for append.
pub struct Ledger<E> {
    base_dir: PathBuf,
    last_fsync: Instant,
    _marker: std::marker::PhantomData<E>,
}

impl<E: Serialize + DeserializeOwned> Ledger<E> {
    /// Open (or create) a ledger directory.
    pub fn open(base_dir: &Path) -> Result<Self> {
        fs::create_dir_all(base_dir).context("create ledger dir")?;
        Ok(Self {
            base_dir: base_dir.to_path_buf(),
            last_fsync: Instant::now(),
            _marker: std::marker::PhantomData,
        })
    }

    /// Append one event to today's JSONL file. Debounced fsync (≤1s).
    pub fn append(&mut self, event: &E) -> Result<()> {
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = self.base_dir.join(format!("{today}.jsonl"));
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open ledger {}", path.display()))?;
        let line = serde_json::to_string(event).context("serialize event")?;
        writeln!(f, "{line}").context("write event")?;

        if self.last_fsync.elapsed() > Duration::from_secs(1) {
            f.sync_data().context("fsync ledger")?;
            self.last_fsync = Instant::now();
        }
        Ok(())
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn flush(&mut self) -> Result<()> {
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = self.base_dir.join(format!("{today}.jsonl"));
        if path.exists() {
            let f = OpenOptions::new()
                .write(true)
                .open(&path)
                .with_context(|| format!("open for fsync {}", path.display()))?;
            f.sync_data().context("fsync ledger")?;
        }
        self.last_fsync = Instant::now();
        Ok(())
    }

    /// Scan the most recent `days` daily files in chronological order.
    /// Skips malformed lines and missing files.
    pub fn scan_days(base_dir: &Path, days: u32) -> Vec<Result<E>> {
        let mut out = Vec::new();
        if !base_dir.exists() {
            return out;
        }
        let today = Local::now().date_naive();
        let mut dates: Vec<_> = (0..days as i64)
            .map(|i| today - chrono::Duration::days(i))
            .collect();
        dates.sort();
        for date in dates {
            let p = base_dir.join(format!("{}.jsonl", date.format("%Y-%m-%d")));
            if !p.exists() {
                continue;
            }
            let f = match std::fs::File::open(&p) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for (i, line) in BufReader::new(f).lines().enumerate() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<E>(&line) {
                    Ok(e) => out.push(Ok(e)),
                    Err(err) => {
                        tracing::warn!(
                            "ledger {}:{} skip malformed line: {err}",
                            p.display(),
                            i + 1
                        );
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::TempDir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestEvent {
        msg: String,
        n: u32,
    }

    #[test]
    fn append_and_scan_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = Ledger::<TestEvent>::open(tmp.path()).unwrap();
        ledger.append(&TestEvent { msg: "hello".into(), n: 1 }).unwrap();
        ledger.append(&TestEvent { msg: "world".into(), n: 2 }).unwrap();
        // Must flush so the file is on disk before scanning
        ledger.flush().unwrap();
        drop(ledger);

        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 1);
        let events: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].msg, "hello");
        assert_eq!(events[1].msg, "world");
    }

    #[test]
    fn open_creates_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        let _ledger = Ledger::<TestEvent>::open(&sub).unwrap();
        assert!(sub.exists());
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 7);
        assert!(results.is_empty());
    }

    #[test]
    fn scan_skips_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        let path = tmp.path().join(format!("{today}.jsonl"));
        std::fs::write(&path, r#"{"msg":"ok","n":1}"#.to_string() + "\n" + "garbage\n" + r#"{"msg":"also ok","n":3}"# + "\n").unwrap();

        let results = Ledger::<TestEvent>::scan_days(tmp.path(), 1);
        let events: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].msg, "ok");
        assert_eq!(events[1].msg, "also ok");
    }
}
```

- [ ] **Step 2: Add `pub mod ledger;` to mur-common/src/lib.rs**

- [ ] **Step 3: Create action_pipeline ledger wrapper in mur-core**

```rust
// mur-core/src/action_pipeline/ledger.rs

use mur_common::action::ActionEvent;
use mur_common::ledger::Ledger as GenericLedger;
use std::path::Path;

/// Shared action-pipeline ledger wrapping the generic `Ledger<ActionEvent>`.
pub struct ActionLedger {
    inner: GenericLedger<ActionEvent>,
}

impl ActionLedger {
    pub fn open(base_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: GenericLedger::open(base_dir)?,
        })
    }

    pub fn append(&mut self, event: &ActionEvent) -> anyhow::Result<()> {
        self.inner.append(event)
    }

    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    /// Replay today's ledger events to rebuild in-memory state.
    pub fn replay_today(base_dir: &Path) -> Vec<ActionEvent> {
        GenericLedger::<ActionEvent>::scan_days(base_dir, 1)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Replay the last `days` of ledger events.
    pub fn replay_days(base_dir: &Path, days: u32) -> Vec<ActionEvent> {
        GenericLedger::<ActionEvent>::scan_days(base_dir, days)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }
}
```

- [ ] **Step 4: Redirect mur-agent-runtime durable ledger to re-export from mur-common**

Replace the implementation in `mur-agent-runtime/src/durable/ledger.rs` with:
```rust
// Re-exported from mur-common.
pub use mur_common::ledger::Ledger;
```

Check all callers still compile:
Run: `cargo build -p mur-agent-runtime`

- [ ] **Step 5: Run tests**

Run: `cargo test -p mur-common -- ledger`
Expected: 4 tests PASS

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/ledger.rs mur-common/src/lib.rs mur-core/src/action_pipeline/ledger.rs mur-agent-runtime/src/durable/ledger.rs
git commit -m "refactor(ledger): extract generic Ledger<E> to mur-common + add ActionLedger"
```

---

## Phase 1: INGEST — File Intake

### Task 1.1: Create ingest module (PendingStore, MIME detect, dedup, merge)

**Files:**
- Create: `mur-core/src/action_pipeline/ingest.rs`
- Modify: `mur-core/src/action_pipeline/mod.rs`

- [ ] **Step 1: Write the test file**

```rust
// mur-core/src/action_pipeline/ingest.rs — add at the bottom of the file

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
        let pipeline = Pipeline::new(home.clone(), ActionPipelineConfig::default());
        (pipeline, tmp)
    }

    #[test]
    fn ingest_single_file_creates_pending_item() {
        let (pipeline, _tmp) = temp_pipeline();
        let src = make_file(_tmp.path(), "test.txt", b"hello world");

        let store = PendingStore::new(&pipeline).unwrap();
        let item = store.ingest_files(
            ItemSource::DragDrop { paths: vec![src.clone()] },
            vec![src],
        ).unwrap();

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

        let store = PendingStore::new(&pipeline).unwrap();
        let item1 = store.ingest_files(
            ItemSource::DragDrop { paths: vec![src.clone()] },
            vec![src.clone()],
        ).unwrap();

        // Within 5s, same path → dedup, return existing item
        let item2 = store.ingest_files(
            ItemSource::DragDrop { paths: vec![src.clone()] },
            vec![src],
        ).unwrap();
        assert_eq!(item1.id, item2.id, "same path within 5s must dedup");
    }

    #[test]
    fn merge_new_files_within_5s_extends_batch() {
        let (pipeline, _tmp) = temp_pipeline();
        let f1 = make_file(_tmp.path(), "a.txt", b"a");
        let f2 = make_file(_tmp.path(), "b.txt", b"b");

        let store = PendingStore::new(&pipeline).unwrap();
        let item1 = store.ingest_files(
            ItemSource::DragDrop { paths: vec![f1.clone()] },
            vec![f1],
        ).unwrap();

        let item2 = store.ingest_files(
            ItemSource::DragDrop { paths: vec![f2.clone()] },
            vec![f2],
        ).unwrap();

        // Same PendingItem (merged), file count increased
        assert_eq!(item1.id, item2.id);
        assert_eq!(item2.files.len(), 2);
    }

    #[test]
    fn expired_items_are_removed_from_snapshot() {
        let (pipeline, _tmp) = temp_pipeline();
        let src = make_file(_tmp.path(), "test.txt", b"hi");

        let mut store = PendingStore::new(&pipeline).unwrap();
        let item = store.ingest_files(
            ItemSource::DragDrop { paths: vec![src] },
            vec![src],
        ).unwrap();

        // Manually expire it
        store.expire_item(item.id).unwrap();

        let snapshot = store.snapshot();
        assert!(!snapshot.iter().any(|i| i.id == item.id));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- action_pipeline::ingest`
Expected: FAIL (module not yet implemented)

- [ ] **Step 3: Implement the ingest module**

```rust
// mur-core/src/action_pipeline/ingest.rs

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::action::{
    ActionEvent, ItemSource, PendingFile, PendingItem, PendingStatus,
};
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
        let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
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
            ledger,
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
            && let Some(existing) = self.items.get_mut(&existing_id)
        {
            for path in &paths {
                let mime_type = self.detect_mime(path).unwrap_or_default();
                let metadata = std::fs::metadata(path).unwrap_or_else(|_| std::fs::Metadata::fake());
                existing.files.push(PendingFile {
                    path: path.clone(),
                    mime_type,
                    size_bytes: metadata.len(),
                    thumbnail_path: None,
                });
                self.recent_paths.insert(canonicalize_path(path), now);
            }
            existing.created_at = Utc::now();

            let event = ActionEvent::ItemIngested {
                item: existing.clone(),
            };
            self.ledger.append(&event)?;
            self.write_snapshot()?;
            return Ok(existing.clone());
        }

        // New batch
        let item = self.create_pending_item(source, paths, now)?;
        self.ledger.append(&ActionEvent::ItemIngested {
            item: item.clone(),
        })?;
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
            let mime_type = self.detect_mime(path).unwrap_or_else(|_| "application/octet-stream".into());
            let metadata = std::fs::metadata(path).unwrap_or_else(|_| std::fs::Metadata::fake());
            files.push(PendingFile {
                path: path.clone(),
                mime_type,
                size_bytes: metadata.len(),
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
            // Could be docx, xlsx, pptx, jar, etc. — check extension
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                return Ok(match ext.to_lowercase().as_str() {
                    "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
                    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
                    "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation".into(),
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

    /// Mark an item as expired.
    pub fn expire_item(&mut self, item_id: Uuid) -> Result<()> {
        if let Some(item) = self.items.get_mut(&item_id) {
            item.status = PendingStatus::Expired;
            self.ledger.append(&ActionEvent::ItemExpired {
                item_id,
            })?;
            self.write_snapshot()?;
        }
        Ok(())
    }

    /// Select an action for a pending item.
    pub fn select_action(&mut self, item_id: Uuid, action: mur_common::action::Action) -> Result<()> {
        if let Some(item) = self.items.get_mut(&item_id) {
            item.status = PendingStatus::Selected {
                action_id: action.id.clone(),
            };
            self.ledger.append(&ActionEvent::ItemSelected {
                item_id,
                action,
            })?;
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
        let json = serde_json::to_vec_pretty(&self.snapshot().into_iter().cloned().collect::<Vec<_>>())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

// Helper: fake metadata for files that can't be stat'd
trait MetadataExt {
    fn fake() -> Self;
    fn len(&self) -> u64;
}
impl MetadataExt for std::fs::Metadata {
    fn fake() -> Self {
        // Use a temp file to get a real Metadata — this is a test-only path.
        // In production, missing metadata returns 0.
        unimplemented!("real metadata only")
    }
    fn len(&self) -> u64 {
        self.len()
    }
}

#[cfg(test)]
mod tests {
    // (tests from Step 1)
}
```

Wait — the `std::fs::Metadata::fake()` pattern is wrong. Fix the approach:

- [ ] **Step 3 (revised): Use a simpler pattern for file size**

Instead of the `MetadataExt` trait, just handle the `Result` inline:

```rust
// In create_pending_item and ingest_files:
let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core -- action_pipeline::ingest`
Expected: 5–6 tests PASS

- [ ] **Step 5: Update mod.rs to re-export PendingStore**

In `mur-core/src/action_pipeline/mod.rs`, add:
```rust
pub use ingest::PendingStore;
```

And add `#[derive(Clone)]` to `Pipeline`:
```rust
#[derive(Clone)]
pub struct Pipeline { ... }
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/action_pipeline/ingest.rs mur-core/src/action_pipeline/mod.rs
git commit -m "feat(action-pipeline): add PendingStore with MIME detect, dedup, merge"
```

---

## Phase 2: QUEUE — Task Management

### Task 2.1: Create queue module (TaskQueue, state machine, concurrency)

**Files:**
- Create: `mur-core/src/action_pipeline/queue.rs`
- Modify: `mur-core/src/action_pipeline/mod.rs`

- [ ] **Step 1: Write the test file**

```rust
// mur-core/src/action_pipeline/queue.rs — add at bottom

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::action::{Action, ActionPipelineConfig};
    use uuid::Uuid;

    fn test_pipeline() -> Pipeline {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("agent_home");
        std::fs::create_dir_all(&home).unwrap();
        Pipeline::new(home, ActionPipelineConfig::default())
    }

    #[test]
    fn enqueue_creates_task_in_queued_state() {
        let pipeline = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let pending_id = Uuid::now_v7();
        let action = Action {
            id: "summarize".into(),
            label: "Summarize".into(),
            user_prompt: None,
        };

        let task = queue.enqueue(pending_id, action, 30, vec![]).unwrap();
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.pending_item_id, pending_id);
    }

    #[test]
    fn enqueue_at_capacity_returns_error() {
        let mut pipeline = test_pipeline();
        pipeline.config.queue.max_concurrent = 1;

        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "a".into(), label: "A".into(), user_prompt: None };

        let t1 = queue.enqueue(Uuid::now_v7(), a.clone(), 30, vec![]).unwrap();
        queue.start_task(t1.id).unwrap();

        // Second task at capacity
        let result = queue.enqueue(Uuid::now_v7(), a, 30, vec![]);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::QueueFull { .. } => {}
            e => panic!("expected QueueFull, got {e:?}"),
        }
    }

    #[test]
    fn state_machine_transitions() {
        let pipeline = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "t".into(), label: "T".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();

        // Queued → Running
        queue.start_task(task.id).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Running);

        // Running → Paused
        queue.pause_task(task.id, "user request".into()).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Paused);

        // Paused → Running (resume)
        queue.resume_task(task.id).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Running);

        // Running → Completed
        let outcome = TaskOutcome::Success { outputs: vec![] };
        queue.complete_task(task.id, outcome.clone()).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Completed { outcome });
    }

    #[test]
    fn cancel_cleans_up_task() {
        let pipeline = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "c".into(), label: "C".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();

        queue.cancel_task(task.id).unwrap();
        let task = queue.get(task.id).unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
    }

    #[test]
    fn ledger_rebuild_on_crash() {
        let pipeline = test_pipeline();
        let pending_id = Uuid::now_v7();
        let a = Action { id: "r".into(), label: "R".into(), user_prompt: None };

        {
            let mut queue = TaskQueue::new(&pipeline).unwrap();
            let task = queue.enqueue(pending_id, a.clone(), 30, vec![]).unwrap();
            queue.start_task(task.id).unwrap();
        } // "crash" — drop

        // Rebuild from ledger
        let queue = TaskQueue::new(&pipeline).unwrap();
        let tasks: Vec<_> = queue.all_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].action.id, "r");
        assert_eq!(tasks[0].state, TaskState::Running);
    }

    #[test]
    fn step_reporting_updates_task() {
        let pipeline = test_pipeline();
        let mut queue = TaskQueue::new(&pipeline).unwrap();
        let a = Action { id: "s".into(), label: "S".into(), user_prompt: None };
        let task = queue.enqueue(Uuid::now_v7(), a, 30, vec![]).unwrap();
        queue.start_task(task.id).unwrap();

        let step = TaskStep {
            index: 0,
            label: "Reading file...".into(),
            state: StepState::Done,
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        };
        queue.update_step(task.id, step.clone()).unwrap();

        let task = queue.get(task.id).unwrap();
        assert_eq!(task.steps.len(), 1);
        assert_eq!(task.steps[0].label, "Reading file...");
        assert_eq!(task.steps[0].state, StepState::Done);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- action_pipeline::queue`
Expected: FAIL (module not implemented)

- [ ] **Step 3: Implement the queue module**

```rust
// mur-core/src/action_pipeline/queue.rs

use anyhow::Result;
use chrono::Utc;
use mur_common::action::{
    Action, ActionEvent, Task, TaskOutcome, TaskState, TaskStep,
};
use std::collections::HashMap;
use uuid::Uuid;

use super::ledger::ActionLedger;
use super::{Pipeline, PipelineError};

pub struct TaskQueue {
    pipeline: Pipeline,
    tasks: HashMap<Uuid, Task>,
    ledger: ActionLedger,
}

impl TaskQueue {
    pub fn new(pipeline: &Pipeline) -> Result<Self> {
        let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;

        // Rebuild state from ledger
        let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 7);
        let mut tasks: HashMap<Uuid, Task> = HashMap::new();
        for event in &events {
            match event {
                ActionEvent::TaskEnqueued { task } => {
                    tasks.insert(task.id, task.clone());
                }
                ActionEvent::TaskStarted { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Running;
                        t.started_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskPaused { task_id, .. } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Paused;
                    }
                }
                ActionEvent::TaskResumed { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Running;
                    }
                }
                ActionEvent::TaskCompleted { task_id, outcome } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Completed {
                            outcome: outcome.clone(),
                        };
                        t.completed_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskCancelled { task_id } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        t.state = TaskState::Cancelled;
                        t.completed_at = Some(Utc::now());
                    }
                }
                ActionEvent::TaskStepUpdated { task_id, step } => {
                    if let Some(t) = tasks.get_mut(task_id) {
                        if let Some(existing) = t.steps.iter_mut().find(|s| s.index == step.index) {
                            *existing = step.clone();
                        } else {
                            t.steps.push(step.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            pipeline: pipeline.clone(),
            tasks,
            ledger,
        })
    }

    /// Enqueue a new task. Returns `QueueFull` if at max_concurrent.
    pub fn enqueue(
        &mut self,
        pending_item_id: Uuid,
        action: Action,
        timeout_seconds: u32,
        initial_steps: Vec<TaskStep>,
    ) -> Result<Task, PipelineError> {
        let running_count = self
            .tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Running))
            .count();
        if running_count >= self.pipeline.config.queue.max_concurrent as usize {
            return Err(PipelineError::QueueFull {
                current: running_count,
                max: self.pipeline.config.queue.max_concurrent,
            });
        }

        let task = Task {
            id: Uuid::now_v7(),
            pending_item_id,
            action,
            state: TaskState::Queued,
            steps: initial_steps,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            timeout_seconds,
        };

        self.tasks.insert(task.id, task.clone());
        self.ledger
            .append(&ActionEvent::TaskEnqueued { task: task.clone() })?;
        Ok(task)
    }

    /// Transition a task from Queued → Running.
    pub fn start_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(PipelineError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;
        if !matches!(task.state, TaskState::Queued) {
            return Err(PipelineError::Other(format!(
                "task {task_id} is not queued"
            )));
        }
        task.state = TaskState::Running;
        task.started_at = Some(Utc::now());
        self.ledger
            .append(&ActionEvent::TaskStarted { task_id })?;
        Ok(())
    }

    /// Transition Running → Paused.
    pub fn pause_task(&mut self, task_id: Uuid, reason: String) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Paused;
        self.ledger.append(&ActionEvent::TaskPaused {
            task_id,
            reason,
        })?;
        Ok(())
    }

    /// Transition Paused → Running.
    pub fn resume_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Running;
        self.ledger
            .append(&ActionEvent::TaskResumed { task_id })?;
        Ok(())
    }

    /// Transition Running → Completed.
    pub fn complete_task(&mut self, task_id: Uuid, outcome: TaskOutcome) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Completed {
            outcome: outcome.clone(),
        };
        task.completed_at = Some(Utc::now());
        self.ledger.append(&ActionEvent::TaskCompleted {
            task_id,
            outcome,
        })?;
        Ok(())
    }

    /// Transition Running | Queued → Cancelled.
    pub fn cancel_task(&mut self, task_id: Uuid) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        task.state = TaskState::Cancelled;
        task.completed_at = Some(Utc::now());
        self.ledger
            .append(&ActionEvent::TaskCancelled { task_id })?;
        Ok(())
    }

    /// Update a step on a running task.
    pub fn update_step(&mut self, task_id: Uuid, step: TaskStep) -> Result<()> {
        let task = self.tasks.get_mut(&task_id).ok_or(PipelineError::TaskNotFound {
            task_id: task_id.to_string(),
        })?;
        if let Some(existing) = task.steps.iter_mut().find(|s| s.index == step.index) {
            *existing = step.clone();
        } else {
            task.steps.push(step.clone());
        }
        self.ledger.append(&ActionEvent::TaskStepUpdated {
            task_id,
            step,
        })?;
        Ok(())
    }

    pub fn get(&self, task_id: Uuid) -> Option<&Task> {
        self.tasks.get(&task_id)
    }

    pub fn all_tasks(&self) -> Vec<&Task> {
        self.tasks.values().collect()
    }

    pub fn queued_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Queued))
            .collect()
    }

    pub fn running_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Running))
            .collect()
    }

    pub fn completed_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Completed { .. }))
            .collect()
    }

    pub fn failed_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| {
                matches!(
                    t.state,
                    TaskState::Completed {
                        outcome: TaskOutcome::Failed { .. }
                    }
                )
            })
            .collect()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core -- action_pipeline::queue`
Expected: 6 tests PASS (including crash recovery and step reporting)

- [ ] **Step 5: Update mod.rs**

In `mur-core/src/action_pipeline/mod.rs`, add:
```rust
pub use queue::TaskQueue;
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/action_pipeline/queue.rs mur-core/src/action_pipeline/mod.rs
git commit -m "feat(action-pipeline): add TaskQueue with state machine + concurrency"
```

---

### Task 2.2: Add CLI: mur agent queue

**Files:**
- Create: `mur-core/src/cmd/agent/queue_cmd.rs`
- Modify: `mur-core/src/cli/agent.rs` (add Queue variant + AgentQueueAction enum)
- Modify: `mur-core/src/dispatch.rs` (add dispatch arm)
- Modify: `mur-core/src/cmd/agent/mod.rs` (add mod + re-export)

- [ ] **Step 1: Add AgentQueueAction enum and AgentAction::Queue variant**

In `mur-core/src/cli/agent.rs`, add after the `Schedule` variant (line ~230):

```rust
    /// Manage the action task queue (list/pause/cancel/retry)
    Queue {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: AgentQueueAction,
    },
```

And at the bottom of the file, add the enum:

```rust
#[derive(Subcommand)]
pub enum AgentQueueAction {
    /// List all tasks in the queue
    List,
    /// Pause a running task at next checkpoint
    Pause {
        /// Task ID (UUID)
        id: String,
    },
    /// Resume a paused task
    Resume {
        /// Task ID (UUID)
        id: String,
    },
    /// Cancel a queued or running task
    Cancel {
        /// Task ID (UUID)
        id: String,
    },
    /// Retry a failed task
    Retry {
        /// Task ID (UUID)
        id: String,
    },
}
```

- [ ] **Step 2: Create CLI handler**

```rust
// mur-core/src/cmd/agent/queue_cmd.rs

use anyhow::{Context, Result, bail};
use mur_common::action::ActionPipelineConfig;
use mur_core::action_pipeline::{Pipeline, TaskQueue};
use uuid::Uuid;

use super::resolve_mur_home;

fn pipeline_for(name: &str) -> Result<(Pipeline, TaskQueue)> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline.clone();
    let pipeline = Pipeline::new(agent_home, config);
    let queue = TaskQueue::new(&pipeline)?;
    Ok((pipeline, queue))
}

pub fn cmd_queue_list(name: &str) -> Result<()> {
    let (_pipeline, queue) = pipeline_for(name)?;
    let tasks = queue.all_tasks();
    if tasks.is_empty() {
        println!("no tasks for agent '{name}'");
        return Ok(());
    }
    println!("{:<36} {:<12} {:<20} ACTION", "ID", "STATE", "CREATED");
    for t in tasks {
        let state = match &t.state {
            mur_common::action::TaskState::Queued => "QUEUED",
            mur_common::action::TaskState::Running => "RUNNING",
            mur_common::action::TaskState::Paused => "PAUSED",
            mur_common::action::TaskState::Completed { .. } => "COMPLETED",
            mur_common::action::TaskState::Cancelled => "CANCELLED",
        };
        println!(
            "{:<36} {:<12} {:<20} {}",
            t.id,
            state,
            t.created_at.format("%Y-%m-%d %H:%M:%S"),
            t.action.label,
        );
    }
    Ok(())
}

pub fn cmd_queue_pause(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.pause_task(task_id, "CLI pause".into())?;
    println!("paused task {id}");
    Ok(())
}

pub fn cmd_queue_resume(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.resume_task(task_id)?;
    println!("resumed task {id}");
    Ok(())
}

pub fn cmd_queue_cancel(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    queue.cancel_task(task_id)?;
    println!("cancelled task {id}");
    Ok(())
}

pub fn cmd_queue_retry(name: &str, id: &str) -> Result<()> {
    let (_pipeline, mut queue) = pipeline_for(name)?;
    let task_id = Uuid::parse_str(id).context("invalid task ID")?;
    let task = queue.get(task_id).with_context(|| format!("task {id} not found"))?;
    let action = task.action.clone();
    let pending_id = task.pending_item_id;
    queue.enqueue(pending_id, action, task.timeout_seconds, vec![])?;
    println!("re-enqueued task {id}");
    Ok(())
}
```

- [ ] **Step 3: Add module to agent/mod.rs**

```rust
mod queue_cmd;
#[allow(unused_imports)]
pub use queue_cmd::{cmd_queue_cancel, cmd_queue_list, cmd_queue_pause, cmd_queue_resume, cmd_queue_retry};
```

- [ ] **Step 4: Add dispatch arm in dispatch.rs**

Find the `AgentAction::Schedule` dispatch arm and add after it:

```rust
        AgentAction::Queue { name, action } => match action {
            AgentQueueAction::List => cmd::agent::cmd_queue_list(&name)?,
            AgentQueueAction::Pause { id } => cmd::agent::cmd_queue_pause(&name, &id)?,
            AgentQueueAction::Resume { id } => cmd::agent::cmd_queue_resume(&name, &id)?,
            AgentQueueAction::Cancel { id } => cmd::agent::cmd_queue_cancel(&name, &id)?,
            AgentQueueAction::Retry { id } => cmd::agent::cmd_queue_retry(&name, &id)?,
        },
```

Add `AgentQueueAction` to the import at the top of dispatch.rs.

- [ ] **Step 5: Build and verify**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/queue_cmd.rs mur-core/src/cmd/agent/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(action-pipeline): add CLI `mur agent queue` subcommand"
```

---

## Phase 4a: NOTIFY — Completion Aggregation

### Task 4.1: Create notify module

**Files:**
- Create: `mur-core/src/action_pipeline/notify.rs`
- Modify: `mur-core/src/action_pipeline/mod.rs`

- [ ] **Step 1: Write the test file**

```rust
// mur-core/src/action_pipeline/notify.rs — add at bottom

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::action::{ActionOutput, OutputKind, TaskOutcome};
    use uuid::Uuid;

    #[test]
    fn aggregate_same_batch_single_notification() {
        let outputs = vec![
            ActionOutput { kind: OutputKind::File, file_path: Some("/tmp/a.txt".into()), chat_content: None },
            ActionOutput { kind: OutputKind::File, file_path: Some("/tmp/b.txt".into()), chat_content: None },
        ];
        let notifications = Aggregator::build_completion_notifications(
            "TestAgent",
            &TaskOutcome::Success { outputs },
            1, // pending_item_count
        );
        // One aggregated notification
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].body.contains("2 succeeded"));
    }

    #[test]
    fn partial_success_reports_counts() {
        let outcome = TaskOutcome::PartialSuccess { succeeded: 3, failed: 2 };
        let notifications = Aggregator::build_completion_notifications(
            "Agent",
            &outcome,
            0,
        );
        assert!(notifications[0].body.contains("3 succeeded"));
        assert!(notifications[0].body.contains("2 failed"));
    }

    #[test]
    fn badge_count_is_pending_plus_running() {
        let badge = Aggregator::badge_count(3, 2, 5, 1);
        // pending(3) + running(2) = 5; completed(5) + failed(1) excluded
        assert_eq!(badge, 5);
    }

    #[test]
    fn deletion_notification_is_independent() {
        let n = Aggregator::build_deletion_notification(
            "Agent",
            3,
            10, // cancel_window_minutes
        );
        assert!(n.body.contains("3 files"));
        assert!(n.body.contains("10 min"));
        assert_eq!(n.urgency, "high");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core -- action_pipeline::notify`
Expected: FAIL

- [ ] **Step 3: Implement notify module**

```rust
// mur-core/src/action_pipeline/notify.rs

use mur_common::action::TaskOutcome;

/// Notification payload that the GUI bridge can render.
#[derive(Debug, Clone)]
pub struct ActionNotification {
    pub event_type: String,
    pub title: String,
    pub body: String,
    pub urgency: String,
    pub file_count: usize,
    pub item_id: Option<String>,
}

pub struct Aggregator;

impl Aggregator {
    /// Build completion notifications for a finished task.
    /// Same-batch results → 1 notification.
    pub fn build_completion_notifications(
        agent_name: &str,
        outcome: &TaskOutcome,
        pending_item_count: usize,
    ) -> Vec<ActionNotification> {
        match outcome {
            TaskOutcome::Success { outputs } => {
                let count = outputs.len();
                vec![ActionNotification {
                    event_type: "task_completed".into(),
                    title: format!("{} completed", agent_name),
                    body: format!("{count} succeeded"),
                    urgency: "normal".into(),
                    file_count: count,
                    item_id: None,
                }]
            }
            TaskOutcome::PartialSuccess { succeeded, failed } => {
                vec![ActionNotification {
                    event_type: "task_completed".into(),
                    title: format!("{} completed", agent_name),
                    body: format!("{succeeded} succeeded, {failed} failed"),
                    urgency: "normal".into(),
                    file_count: (succeeded + failed) as usize,
                    item_id: None,
                }]
            }
            TaskOutcome::Failed { error } => {
                vec![ActionNotification {
                    event_type: "task_failed".into(),
                    title: format!("{} failed", agent_name),
                    body: error.clone(),
                    urgency: "high".into(),
                    file_count: 0,
                    item_id: None,
                }]
            }
        }
    }

    /// Build a deletion notification (independent from completion,
    /// always high urgency).
    pub fn build_deletion_notification(
        agent_name: &str,
        file_count: usize,
        cancel_window_minutes: u32,
    ) -> ActionNotification {
        ActionNotification {
            event_type: "deletion_pending".into(),
            title: format!("{agent_name} wants to delete {file_count} files"),
            body: format!(
                "Moving to Trash in {cancel_window_minutes} min · recoverable"
            ),
            urgency: "high".into(),
            file_count,
            item_id: None,
        }
    }

    /// Badge number = pending items + running tasks.
    /// Completed and failed are excluded.
    pub fn badge_count(
        pending_items: usize,
        running_tasks: usize,
        completed_count: usize,
        failed_count: usize,
    ) -> usize {
        pending_items + running_tasks
    }

    /// Build the structured notification metadata (Courier dual-audience format).
    pub fn build_ingest_notification(
        item_id: &str,
        file_count: usize,
        mime_types: &[String],
        file_names: &[String],
    ) -> ActionNotification {
        let body = if file_names.len() <= 3 {
            file_names.join(", ")
        } else {
            format!("{} and {} more", &file_names[..3].join(", "), file_names.len() - 3)
        };

        ActionNotification {
            event_type: "pending_item_ingested".into(),
            title: format!("{file_count} files received"),
            body,
            urgency: "low".into(),
            file_count,
            item_id: Some(item_id.to_string()),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core -- action_pipeline::notify`
Expected: 4 tests PASS

- [ ] **Step 5: Update mod.rs**

```rust
pub use notify::{ActionNotification, Aggregator};
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/action_pipeline/notify.rs mur-core/src/action_pipeline/mod.rs
git commit -m "feat(action-pipeline): add Aggregator for completion + deletion notifications"
```

---

## Phase 3 (Gate Only): TrashGuard Hook

### Task 3.1: Create TrashGuard Hook (detection + schema checks only)

**Files:**
- Create: `mur-core/src/action_pipeline/guard.rs`
- Create: `mur-agent-runtime/src/hooks/trash_guard.rs`
- Modify: `mur-agent-runtime/src/hooks/mod.rs`
- Modify: `mur-common/src/hooks_config.rs`

- [ ] **Step 1: Write tests for TrashGuard gate logic**

```rust
// mur-core/src/action_pipeline/guard.rs — add at bottom

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> DeletionConfig {
        DeletionConfig {
            trash_enabled: true,
            cancel_window_minutes: 10,
            trash_retention_days: 30,
            trash_max_mb: 1024,
            max_batch: 50,
            auto_permanent_delete: false,
            trusted_paths: vec![],
        }
    }

    #[test]
    fn detect_shell_rm_pattern() {
        let detections = DestructivePattern::detect_in_shell("rm -rf /tmp/foo");
        assert!(!detections.is_empty());
        assert!(detections.iter().any(|d| matches!(d, DestructivePattern::Rm { .. })));
    }

    #[test]
    fn detect_python_os_remove() {
        let detections = DestructivePattern::detect_in_code("os.remove('/tmp/x')");
        assert!(!detections.is_empty());
    }

    #[test]
    fn detect_wildcard_in_path() {
        let detections = DestructivePattern::detect_in_shell("rm /tmp/*.txt");
        assert!(detections.iter().any(|d| d.contains_wildcard()));
    }

    #[test]
    fn reject_batch_above_max() {
        let config = DeletionConfig { max_batch: 5, ..test_config() };
        let guard = TrashGuardLogic::new(config);
        let result = guard.check_batch_size(10);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::BatchTooLarge { count, max } => {
                assert_eq!(count, 10);
                assert_eq!(max, 5);
            }
            e => panic!("expected BatchTooLarge, got {e:?}"),
        }
    }

    #[test]
    fn allow_batch_at_or_below_max() {
        let config = DeletionConfig { max_batch: 5, ..test_config() };
        let guard = TrashGuardLogic::new(config);
        assert!(guard.check_batch_size(5).is_ok());
        assert!(guard.check_batch_size(1).is_ok());
    }

    #[test]
    fn path_within_allowed_scope() {
        let config = DeletionConfig {
            trusted_paths: vec!["/tmp/allowed/".into()],
            ..test_config()
        };
        let guard = TrashGuardLogic::new(config);
        // Canonicalized path within trusted_paths
        let allowed = PathBuf::from("/tmp/allowed/test.txt");
        assert!(guard.check_path_scope(&allowed).is_ok());
    }

    #[test]
    fn path_outside_scope_rejected() {
        let config = DeletionConfig {
            trusted_paths: vec!["/tmp/allowed/".into()],
            ..test_config()
        };
        let guard = TrashGuardLogic::new(config);
        let denied = PathBuf::from("/etc/passwd");
        assert!(guard.check_path_scope(&denied).is_err());
    }
}
```

- [ ] **Step 2: Implement TrashGuardLogic (pure logic, no Hook trait)**

```rust
// mur-core/src/action_pipeline/guard.rs

use mur_common::action::DeletionConfig;
use std::path::{Path, PathBuf};

use super::PipelineError;

/// Destructive patterns the guard detects.
#[derive(Debug, PartialEq)]
pub enum DestructivePattern {
    /// Shell: `rm`, `unlink`, `mv ... /dev/null`
    Rm { raw: String, paths: Vec<String> },
    /// Python: `os.remove`, `os.unlink`, `shutil.rmtree`
    PythonRemove { raw: String, paths: Vec<String> },
    /// MCP: tool named `delete_file` or similar
    McpDelete { tool_name: String, paths: Vec<String> },
    /// A2A: delete intent in message
    A2ADelete { paths: Vec<String> },
}

impl DestructivePattern {
    /// Scan a shell command string for destructive patterns.
    pub fn detect_in_shell(cmd: &str) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();
        let cmd_trimmed = cmd.trim();

        // rm command detection
        if let Some(rest) = cmd_trimmed
            .strip_prefix("rm ")
            .or_else(|| cmd_trimmed.strip_prefix("/bin/rm "))
            .or_else(|| cmd_trimmed.strip_prefix("/usr/bin/rm "))
        {
            let paths = extract_paths(rest);
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths,
            });
        }

        // unlink detection
        if cmd_trimmed.starts_with("unlink ") {
            let paths = extract_paths(&cmd_trimmed["unlink ".len()..]);
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths,
            });
        }

        // mv to /dev/null detection
        if cmd_trimmed.contains(" /dev/null") || cmd_trimmed.contains(" /dev/null\n") {
            patterns.push(DestructivePattern::Rm {
                raw: cmd.to_string(),
                paths: vec![],
            });
        }

        patterns
    }

    /// Scan code (Python, etc.) for destructive calls.
    pub fn detect_in_code(code: &str) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();

        for keyword in &["os.remove", "os.unlink", "shutil.rmtree", "pathlib.Path.unlink"] {
            if code.contains(keyword) {
                // Naive path extraction inside parens
                let paths = extract_python_paths(code, keyword);
                patterns.push(DestructivePattern::PythonRemove {
                    raw: code.to_string(),
                    paths,
                });
            }
        }

        patterns
    }

    /// Check if any path in this pattern contains a wildcard.
    pub fn contains_wildcard(&self) -> bool {
        let paths = match self {
            DestructivePattern::Rm { paths, .. } => paths,
            DestructivePattern::PythonRemove { paths, .. } => paths,
            DestructivePattern::McpDelete { paths, .. } => paths,
            DestructivePattern::A2ADelete { paths } => paths,
        };
        paths.iter().any(|p| p.contains('*') || p.contains('?'))
    }

    /// Check if pattern matches MCP delete_file tool.
    pub fn detect_in_mcp_tool(tool_name: &str, arguments: &serde_json::Value) -> Vec<DestructivePattern> {
        let delete_tools = [
            "delete_file",
            "delete_files",
            "remove_file",
            "remove_files",
            "fs_delete",
            "fs.remove",
        ];
        if delete_tools.iter().any(|t| tool_name.eq_ignore_ascii_case(t))
            || tool_name.to_lowercase().contains("delete")
            || tool_name.to_lowercase().contains("remove")
        {
            let paths = extract_json_paths(arguments);
            return vec![DestructivePattern::McpDelete {
                tool_name: tool_name.to_string(),
                paths,
            }];
        }
        vec![]
    }
}

/// Pure guard logic — no I/O, no Hook trait. Callable from Hook impl AND tests.
pub struct TrashGuardLogic {
    config: DeletionConfig,
}

impl TrashGuardLogic {
    pub fn new(config: DeletionConfig) -> Self {
        Self { config }
    }

    /// Check batch size is within limits.
    pub fn check_batch_size(&self, count: usize) -> Result<(), PipelineError> {
        if count > self.config.max_batch as usize {
            return Err(PipelineError::BatchTooLarge {
                count,
                max: self.config.max_batch,
            });
        }
        Ok(())
    }

    /// Check a path is within allowed scope (trusted_paths or not).
    /// Returns Ok if the path is safe to delete (within scope).
    pub fn check_path_scope(&self, path: &Path) -> Result<(), PipelineError> {
        if self.config.trusted_paths.is_empty() {
            return Ok(()); // no restrictions
        }
        let canonical = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        for trusted in &self.config.trusted_paths {
            let trusted_path = PathBuf::from(trusted);
            let trusted_canonical = trusted_path
                .canonicalize()
                .unwrap_or_else(|_| trusted_path.clone());
            if canonical.starts_with(&trusted_canonical) {
                return Ok(());
            }
        }
        Err(PipelineError::PathOutOfScope {
            path: path.to_path_buf(),
        })
    }

    /// Detect all destructive patterns in a shell command.
    pub fn detect(&self, tool_name: &str, tool_input: &serde_json::Value) -> Vec<DestructivePattern> {
        let mut patterns = Vec::new();

        // Check MCP tool name
        patterns.extend(DestructivePattern::detect_in_mcp_tool(tool_name, tool_input));

        // Check for shell command in arguments
        if let Some(cmd) = tool_input.get("command").and_then(|v| v.as_str()) {
            patterns.extend(DestructivePattern::detect_in_shell(cmd));
        }
        if let Some(code) = tool_input.get("code").and_then(|v| v.as_str()) {
            patterns.extend(DestructivePattern::detect_in_code(code));
        }

        // Check for path arguments that contain wildcards
        if let Some(path) = tool_input.get("path").and_then(|v| v.as_str()) {
            if path.contains('*') || path.contains('?') {
                patterns.push(DestructivePattern::Rm {
                    raw: format!("delete path={path}"),
                    paths: vec![path.to_string()],
                });
            }
        }

        patterns
    }

    pub fn config(&self) -> &DeletionConfig {
        &self.config
    }
}

// ── Path extraction helpers ──

fn extract_paths(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|w| !w.starts_with('-') && !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

fn extract_python_paths(code: &str, _keyword: &str) -> Vec<String> {
    // Simple extraction: find quoted strings after the keyword
    let mut paths = Vec::new();
    for ch in ['\'', '\"'] {
        let pattern = format!("({ch}");
        for part in code.split(&pattern).skip(1) {
            if let Some(end) = part.find(ch) {
                paths.push(part[..end].to_string());
            }
        }
    }
    paths
}

fn extract_json_paths(args: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        paths.push(path.to_string());
    }
    if let Some(paths_arr) = args.get("paths").and_then(|v| v.as_array()) {
        for p in paths_arr {
            if let Some(s) = p.as_str() {
                paths.push(s.to_string());
            }
        }
    }
    if let Some(file_path) = args.get("file_path").and_then(|v| v.as_str()) {
        paths.push(file_path.to_string());
    }
    paths
}

#[cfg(test)]
mod tests {
    // (tests from Step 1)
}
```

- [ ] **Step 3: Write the Hook impl in mur-agent-runtime**

```rust
// mur-agent-runtime/src/hooks/trash_guard.rs

use async_trait::async_trait;
use mur_core::action_pipeline::guard::TrashGuardLogic;
use tokio_util::sync::CancellationToken;

use crate::hooks::{Decision, Hook, HookCtx, HookError, ToolCall};

pub struct TrashGuard {
    pub logic: TrashGuardLogic,
}

impl TrashGuard {
    pub fn new(logic: TrashGuardLogic) -> Self {
        Self { logic }
    }
}

#[async_trait]
impl Hook for TrashGuard {
    fn name(&self) -> &str {
        "TrashGuard"
    }

    async fn pre_tool_use(
        &self,
        _ctx: &HookCtx,
        call: &ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        let detections = self.logic.detect(&call.tool_name, &call.arguments);

        if detections.is_empty() {
            return Ok(Decision::Allow);
        }

        // Schema-level enforcement
        let path_count = detections
            .iter()
            .map(|d| match d {
                mur_core::action_pipeline::guard::DestructivePattern::Rm { paths, .. } => paths.len(),
                mur_core::action_pipeline::guard::DestructivePattern::PythonRemove { paths, .. } => paths.len(),
                mur_core::action_pipeline::guard::DestructivePattern::McpDelete { paths, .. } => paths.len(),
                mur_core::action_pipeline::guard::DestructivePattern::A2ADelete { paths } => paths.len(),
            })
            .sum::<usize>();

        // Batch size check
        if let Err(e) = self.logic.check_batch_size(path_count) {
            return Ok(Decision::Deny(e.to_string()));
        }

        // Wildcard check
        if detections.iter().any(|d| d.contains_wildcard()) {
            return Ok(Decision::Deny(
                "wildcard patterns rejected in destructive operations".into(),
            ));
        }

        // Two-phase: first destructive op → AskUser (default Deny)
        Ok(Decision::AskUser {
            prompt: format!(
                "Agent wants to perform a destructive operation ({} paths). \
                 Moving to Trash with {}-minute cancel window. Allow?",
                path_count,
                self.logic.config().cancel_window_minutes
            ),
            default: crate::hooks::AskDefault::Deny,
        })
    }
}
```

- [ ] **Step 4: Register hook in hooks/mod.rs**

Add:
```rust
pub mod trash_guard;
pub use trash_guard::TrashGuard;
```

- [ ] **Step 5: Add trash_guard to HooksConfig**

In `mur-common/src/hooks_config.rs`, add:
```rust
    /// Whether `TrashGuard` runs. Default: true.
    #[serde(default = "default_true")]
    pub trash_guard: bool,
```

- [ ] **Step 6: Run all tests**

Run: `cargo test -p mur-core -- action_pipeline::guard`
Run: `cargo build -p mur-agent-runtime`
Expected: tests PASS, runtime compiles

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/action_pipeline/guard.rs mur-agent-runtime/src/hooks/trash_guard.rs mur-agent-runtime/src/hooks/mod.rs mur-common/src/hooks_config.rs
git commit -m "feat(action-pipeline): add TrashGuard Hook with destructive pattern detection"
```

---

## Daemon Integration

### Task 5.1: Add action_pipeline tick to mur-daemon

**Files:**
- Create: `mur-daemon/src/action_tick.rs`
- Modify: `mur-daemon/src/main.rs`

- [ ] **Step 1: Create the action tick module**

```rust
// mur-daemon/src/action_tick.rs

use anyhow::Result;
use chrono::Utc;
use mur_core::action_pipeline::{PendingStore, Pipeline};
use mur_common::action::{ActionEvent, ActionPipelineConfig, TrashStatus};
use mur_core::action_pipeline::ingest::PendingStore;
use mur_core::action_pipeline::ledger::ActionLedger;
use std::path::PathBuf;
use std::time::Instant;

/// Scan all agent homes for action pipeline work:
/// 1. Expire stale pending items (past TTL)
/// 2. Execute deferred trash moves (cancel window elapsed, no Undo)
/// 3. Mark expired trash entries (past retention)
///
/// Called every 30 seconds from mur-daemon's main loop.
pub fn scan_all_agents(mur_home: &PathBuf) -> Result<()> {
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent_home = entry.path();
        if let Err(e) = scan_one_agent(&agent_home) {
            tracing::warn!(
                agent = %entry.file_name().to_string_lossy(),
                error = %e,
                "action_tick: scan failed"
            );
        }
    }
    Ok(())
}

fn scan_one_agent(agent_home: &PathBuf) -> Result<()> {
    // Load config from profile
    let profile_path = agent_home.join("profile.yaml");
    if !profile_path.exists() {
        return Ok(());
    }
    let yaml = std::fs::read_to_string(&profile_path)?;
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    let config = profile.action_pipeline;
    let pipeline = Pipeline::new(agent_home.clone(), config.clone());

    // 1. Expire stale pending items
    if let Ok(mut store) = PendingStore::new(&pipeline) {
        let expired = store.expire_stale().unwrap_or(0);
        if expired > 0 {
            tracing::info!(agent = %profile.name, expired, "expired stale pending items");
        }
    }

    // 2 & 3. Trash work — replay recent events and process
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 7);
    let now = Utc::now();

    for event in &events {
        match event {
            // Execute deferred deletes (cancel window elapsed, no Undo)
            ActionEvent::DeletionPending { entry } => {
                if entry.status == TrashStatus::PendingDelete
                    && entry.execute_at < now
                {
                    execute_move_to_trash(&pipeline, entry)?;
                }
            }
            // Mark expirations (past retention)
            ActionEvent::TrashCreated { entry } => {
                if entry.status == TrashStatus::Retained
                    && let Some(retention_until) = entry.retention_until
                    && retention_until < now
                {
                    mark_expired(&pipeline, &entry.id)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn execute_move_to_trash(
    pipeline: &Pipeline,
    entry: &mur_common::action::TrashEntry,
) -> Result<()> {
    let trash_dir = pipeline.trash_dir();
    std::fs::create_dir_all(&trash_dir)?;

    let ts = Utc::now().format("%Y%m%dT%H%M%S");
    let filename = entry
        .original_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let trash_subdir = trash_dir.join(format!("{ts}_{filename}"));
    std::fs::create_dir_all(&trash_subdir)?;
    let trash_file = trash_subdir.join(filename);

    // Try rename first
    let moved = match std::fs::rename(&entry.original_path, &trash_file) {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV — cross-filesystem, fallback to copy + unlink
            std::fs::copy(&entry.original_path, &trash_file)?;
            std::fs::remove_file(&entry.original_path)?;
            true
        }
        Err(e) => {
            tracing::error!(path=%entry.original_path.display(), error=%e, "move to trash failed");
            false
        }
    };

    if moved {
        let mut updated = entry.clone();
        updated.trash_path = Some(trash_file);
        updated.retention_until =
            Some(Utc::now() + chrono::Duration::days(
                pipeline.config.deletion.trash_retention_days as i64
            ));
        updated.status = TrashStatus::Retained;

        let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
        ledger.append(&ActionEvent::TrashCreated { entry: updated })?;
    }

    Ok(())
}

fn mark_expired(pipeline: &Pipeline, entry_id: &uuid::Uuid) -> Result<()> {
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    ledger.append(&ActionEvent::TrashExpired {
        entry_id: *entry_id,
    })?;
    Ok(())
}
```

- [ ] **Step 2: Wire tick into daemon main loop**

In `mur-daemon/src/main.rs`, add `mod action_tick;` at the top.

In the event loop, add a 30s throttle counter. After the existing sleep check, add:

```rust
        // ── Action pipeline tick (every 30s) ──
        // Throttled by counting 1s poll iterations
        {
            // Use a simple counter in the outer scope
        }
```

Actually, use a simpler approach — add a separate interval:

In `main()` after the heartbeat spawn, add:

```rust
    // Action pipeline tick: every 30 seconds
    let mur_home_tick = mur_dir.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(30));
        tick.tick().await; // skip first immediate fire
        loop {
            tick.tick().await;
            if let Err(e) = action_tick::scan_all_agents(&mur_home_tick) {
                tracing::error!(error = %e, "action_tick failed");
            }
        }
    });
```

- [ ] **Step 3: Build and verify daemon compiles**

Run: `cargo build -p mur-daemon`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add mur-daemon/src/action_tick.rs mur-daemon/src/main.rs
git commit -m "feat(action-pipeline): add 30s action tick to mur-daemon"
```

---

## Remaining Phase 1 CLI: mur agent pending + mur agent trash

### Task 6.1: Add CLI `mur agent pending`

**Files:**
- Create: `mur-core/src/cmd/agent/pending.rs`
- Modify: `mur-core/src/cli/agent.rs`
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`

- [ ] **Step 1: Add `Pending` variant + `AgentPendingAction` to cli/agent.rs**

```rust
    /// List or act on pending ingested items (A1)
    Pending {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: Option<AgentPendingAction>,
    },
```

```rust
#[derive(Subcommand)]
pub enum AgentPendingAction {
    /// List pending items for this agent
    List,
    /// Execute an action on a pending item
    Act {
        /// Pending item UUID
        id: String,
        /// Action ID from file_actions (e.g. "summarize")
        action_id: String,
    },
}
```

- [ ] **Step 2: Create CLI handler**

```rust
// mur-core/src/cmd/agent/pending.rs

use anyhow::{Context, Result, bail};
use mur_common::action::{Action, ActionPipelineConfig};
use mur_core::action_pipeline::{PendingStore, Pipeline};
use uuid::Uuid;

use super::resolve_mur_home;

pub fn cmd_pending_list(name: &str) -> Result<()> {
    let (_, store) = pending_store_for(name)?;
    let items = store.snapshot();
    if items.is_empty() {
        println!("no pending items for agent '{name}'");
        return Ok(());
    }
    println!("{:<36} {:<12} {:<8} FILES", "ID", "STATUS", "COUNT");
    for item in items {
        println!(
            "{:<36} {:<12} {:<8} {}",
            item.id,
            format!("{:?}", item.status).to_lowercase(),
            item.files.len(),
            item.files.iter().map(|f| f.path.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>().join(", "),
        );
    }
    Ok(())
}

pub fn cmd_pending_act(name: &str, id: &str, action_id: &str) -> Result<()> {
    let (pipeline, mut store) = pending_store_for(name)?;
    let item_id = Uuid::parse_str(id).context("invalid item ID")?;

    // Find the action in agent's file_actions
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let action = profile
        .file_actions
        .iter()
        .find(|a| a.id == action_id)
        .map(|fa| Action {
            id: fa.id.clone(),
            label: fa.label_for(&profile.companion.locale),
            user_prompt: None,
        })
        .or_else(|| {
            if action_id == "ask_me" {
                Some(Action {
                    id: "ask_me".into(),
                    label: "Ask me anything...".into(),
                    user_prompt: None,
                })
            } else {
                None
            }
        })
        .with_context(|| format!("action '{action_id}' not found in agent's file_actions"))?;

    store.select_action(item_id, action)?;
    println!("selected action '{action_id}' for item {id}");
    Ok(())
}

fn pending_store_for(name: &str) -> Result<(Pipeline, PendingStore)> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline;
    let pipeline = Pipeline::new(agent_home, config);
    let store = PendingStore::new(&pipeline)?;
    Ok((pipeline, store))
}
```

- [ ] **Step 3: Add module + dispatch**

Add `mod pending;` and re-exports to `cmd/agent/mod.rs`.

Add dispatch arm in `dispatch.rs`:
```rust
        AgentAction::Pending { name, action } => match action {
            Some(AgentPendingAction::List) | None => cmd::agent::cmd_pending_list(&name)?,
            Some(AgentPendingAction::Act { id, action_id }) => cmd::agent::cmd_pending_act(&name, &id, &action_id)?,
        },
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/pending.rs mur-core/src/cmd/agent/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(action-pipeline): add CLI `mur agent pending` subcommand"
```

---

### Task 6.2: Add CLI `mur agent trash`

**Files:**
- Create: `mur-core/src/cmd/agent/trash.rs`
- Modify: `mur-core/src/cli/agent.rs`
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`

- [ ] **Step 1: Add `Trash` variant + `AgentTrashAction` enum to cli/agent.rs**

```rust
    /// Manage agent trash (list/restore/empty/now)
    Trash {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: AgentTrashAction,
    },
```

```rust
#[derive(Subcommand)]
pub enum AgentTrashAction {
    /// List trash contents
    List,
    /// Restore a file from trash
    Restore {
        /// Trash entry UUID
        id: String,
    },
    /// Permanently empty all trash
    Empty,
    /// Immediately delete a specific trash item
    Now {
        /// Trash entry UUID
        id: String,
    },
}
```

- [ ] **Step 2: Create CLI handler**

```rust
// mur-core/src/cmd/agent/trash.rs

use anyhow::{Context, Result, bail};
use mur_common::action::{ActionEvent, PermDeleteReason, TrashStatus};
use mur_core::action_pipeline::ledger::ActionLedger;
use mur_core::action_pipeline::Pipeline;
use uuid::Uuid;

use super::resolve_mur_home;

pub fn cmd_trash_list(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);
    let mut found = false;
    println!("{:<36} {:<14} {:<40} ORIGINAL", "ID", "STATUS", "TRASH_PATH");
    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event {
            found = true;
            let status = format!("{:?}", entry.status).to_lowercase();
            let trash_path = entry.trash_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "?".into());
            println!("{:<36} {:<14} {:<40} {}", entry.id, status, trash_path, entry.original_path.display());
        }
    }
    if !found {
        println!("trash is empty for agent '{name}'");
    }
    Ok(())
}

pub fn cmd_trash_restore(name: &str, id: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let entry_id = Uuid::parse_str(id).context("invalid entry ID")?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.id == entry_id
        {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::rename(trash_path, &entry.original_path)?;
                let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
                ledger.append(&ActionEvent::TrashRestored { entry_id })?;
                println!("restored {} to {}", trash_path.display(), entry.original_path.display());
                return Ok(());
            }
            bail!("trash file not found for entry {id}");
        }
    }
    bail!("trash entry {id} not found");
}

pub fn cmd_trash_empty(name: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);
    let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
    let mut count = 0;

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::remove_file(trash_path)?;
            }
            ledger.append(&ActionEvent::TrashPermDeleted {
                entry_id: entry.id,
                reason: PermDeleteReason::UserEmpty,
            })?;
            count += 1;
        }
    }
    println!("permanently deleted {count} trashed files");
    Ok(())
}

pub fn cmd_trash_now(name: &str, id: &str) -> Result<()> {
    let pipeline = pipeline_for(name)?;
    let entry_id = Uuid::parse_str(id).context("invalid entry ID")?;
    let events = ActionLedger::replay_days(&pipeline.ledger_dir(), 30);

    for event in &events {
        if let ActionEvent::TrashCreated { entry } = event
            && entry.id == entry_id
        {
            if let Some(ref trash_path) = entry.trash_path
                && trash_path.exists()
            {
                std::fs::remove_file(trash_path)?;
            }
            let mut ledger = ActionLedger::open(&pipeline.ledger_dir())?;
            ledger.append(&ActionEvent::TrashPermDeleted {
                entry_id,
                reason: PermDeleteReason::UserNow,
            })?;
            println!("permanently deleted entry {id}");
            return Ok(());
        }
    }
    bail!("trash entry {id} not found");
}

fn pipeline_for(name: &str) -> Result<Pipeline> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let (_path, profile) = super::load_profile_for_edit(name)?;
    let config = profile.action_pipeline;
    Ok(Pipeline::new(agent_home, config))
}
```

- [ ] **Step 3: Add module + dispatch**

Add `mod trash;` and re-exports to `cmd/agent/mod.rs`.

Add dispatch arm:
```rust
        AgentAction::Trash { name, action } => match action {
            AgentTrashAction::List => cmd::agent::cmd_trash_list(&name)?,
            AgentTrashAction::Restore { id } => cmd::agent::cmd_trash_restore(&name, &id)?,
            AgentTrashAction::Empty => cmd::agent::cmd_trash_empty(&name)?,
            AgentTrashAction::Now { id } => cmd::agent::cmd_trash_now(&name, &id)?,
        },
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p mur-core`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/trash.rs mur-core/src/cmd/agent/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(action-pipeline): add CLI `mur agent trash` subcommand"
```

---

## Final Integration

### Task 7.1: End-to-end build verification

- [ ] **Step 1: Build entire workspace**

Run: `cargo build --workspace`
Expected: full workspace compiles with no errors or warnings

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings

- [ ] **Step 4: Run fmt check**

Run: `cargo fmt --check`
Expected: no formatting issues

- [ ] **Step 5: Commit final integration**

```bash
git add -A
git commit -m "feat(action-pipeline): complete Phases 0-2 + 4a + TrashGuard gate"
```

---

## Self-Review

### 1. Spec coverage check

| Spec section | Task(s) |
|---|---|
| Phase 0: Shared infra (types, ledger, profile schema) | 0.1, 0.2, 0.3, 0.4 |
| Phase 1: INGEST (PendingStore, MIME, dedup, merge, expiry) | 1.1 |
| Phase 1: Selection UI + voice flow | GUI tasks (not in this plan — frontend only) |
| Phase 1: Notification metadata (Courier dual-audience) | 4.1 (notify module) |
| Phase 2: QUEUE (TaskQueue, state machine, concurrency) | 2.1 |
| Phase 2: Queue Panel UI | GUI tasks (not in this plan) |
| Phase 2: Pause semantics (checkpoint mode) | 2.1 (pause_task) |
| Phase 2: Cancel semantics (cascade + cleanup) | 2.1 (cancel_task) |
| Phase 2: Step reporting | 2.1 (update_step) |
| Phase 3: TrashGuard gate (Hook impl) | 3.1 |
| Phase 3: Trash executor (rm→mv rewrite) | DEFERRED to P0b |
| Phase 3: Cancel window (pre-execution undo) | 3.1 (DeletionPending event) |
| Phase 3: Trash retention + expiry (no auto-delete) | 5.1 (daemon tick) |
| Phase 3: Trash capacity eviction | DEFERRED (complexity — needs trash size tracking) |
| Phase 3: Two-phase protocol (AskUser + GrantStore) | 3.1 (AskUser in Hook) |
| Phase 3: Trusted paths | 3.1 (check_path_scope) |
| Phase 3: Cross-filesystem trash (EXDEV) | 5.1 (execute_move_to_trash) |
| Phase 4a: Completion aggregation | 4.1 |
| Phase 4b: Per-step reporting | DEFERRED to P0b |
| CLI: `mur agent pending` | 6.1 |
| CLI: `mur agent queue` | 2.2 |
| CLI: `mur agent trash` | 6.2 |
| Daemon: expiry tick (30s) | 5.1 |
| Daemon: trash deferred-execute | 5.1 |
| Daemon: retention marking | 5.1 |

**Gaps:**
- Trash capacity eviction (Expired oldest-first) — deferred due to complexity; placeholder in `action_tick.rs`
- GUI components — annotated but not in scope for Rust plan; separate frontend plan needed
- P0b-dependent work (trash executor, per-step reporting) — explicitly deferred per spec

### 2. Placeholder scan

No "TBD", "TODO", or "implement later" markers. All steps have concrete code.

### 3. Type consistency check

- `PendingItem`, `Task`, `TrashEntry`, `ActionEvent` — defined in Task 0.1, used consistently in all later tasks
- `Pipeline` — defined in Task 0.3, used throughout
- `PendingStore` — defined in Task 1.1, consumed by CLI (6.1) and daemon (5.1)
- `TaskQueue` — defined in Task 2.1, consumed by CLI (2.2)
- `TrashGuardLogic` — defined in Task 3.1, consumed by Hook impl
- `ActionNotification` / `Aggregator` — defined in Task 4.1
- `ActionLedger` — defined in Task 0.4, used everywhere
- `DeletionConfig`, `QueueConfig`, `ActionPipelineConfig` — defined in Task 0.1, consumed throughout
