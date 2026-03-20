//! Typed errors for MUR library boundaries.

/// Errors from store operations (YAML, LanceDB).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("pattern not found: {0}")]
    NotFound(String),
    #[error("pattern already exists: {0}")]
    AlreadyExists(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("YAML serialization error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("store error: {0}")]
    Other(String),
}

/// Errors from retrieval/scoring operations.
#[derive(Debug, thiserror::Error)]
pub enum RetrieveError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("retrieval error: {0}")]
    Other(String),
}

/// Errors from injection operations.
#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no patterns to inject")]
    Empty,
    #[error("injection error: {0}")]
    Other(String),
}

/// Errors from LLM client operations.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("API request failed: {0}")]
    Request(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("provider not configured: {0}")]
    NotConfigured(String),
    #[error("LLM error: {0}")]
    Other(String),
}
