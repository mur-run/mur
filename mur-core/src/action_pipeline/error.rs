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
