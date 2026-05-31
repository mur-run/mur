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
        labels.insert("zh".into(), "摘要".into());
        labels.insert("en".into(), "Summarize".into());
        let fa = FileAction {
            id: "summarize".into(),
            label: labels,
            description: None,
            mime_types: vec!["text/*".into()],
        };
        assert_eq!(fa.label_for("zh-TW"), "摘要"); // exact match on "zh" prefix
        assert_eq!(fa.label_for("zh-CN"), "摘要"); // prefix fallback: zh-CN → zh
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
