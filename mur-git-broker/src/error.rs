#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum BrokerError {
    #[error("invalid_request: {0}")]
    InvalidRequest(String),
    #[error("request_conflict")]
    RequestConflict,
    #[error("queue_full")]
    QueueFull,
    #[error("rate_limited")]
    RateLimited,
    #[error("prefetch_rejected: {0}")]
    PrefetchRejected(String),
    #[error("import_rejected: {0}")]
    ImportRejected(String),
    #[error("not_fast_forward")]
    NotFastForward,
    #[error("ancestry_unprovable: {0}")]
    AncestryUnprovable(String),
    #[error("approval_expired")]
    ApprovalExpired,
    #[error("policy_changed")]
    PolicyChanged,
    #[error("stale_old_sha")]
    StaleOldSha,
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("outcome_unknown: {0}")]
    OutcomeUnknown(String),
    #[error("not_pending")]
    NotPending,
    #[error("storage: {0}")]
    Storage(String),
}
impl BrokerError {
    /// The stable wire name, with no detail (details can carry remote text).
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::RequestConflict => "request_conflict",
            Self::QueueFull => "queue_full",
            Self::RateLimited => "rate_limited",
            Self::PrefetchRejected(_) => "prefetch_rejected",
            Self::ImportRejected(_) => "import_rejected",
            Self::NotFastForward => "not_fast_forward",
            Self::AncestryUnprovable(_) => "ancestry_unprovable",
            Self::ApprovalExpired => "approval_expired",
            Self::PolicyChanged => "policy_changed",
            Self::StaleOldSha => "stale_old_sha",
            Self::Rejected(_) => "rejected",
            Self::OutcomeUnknown(_) => "outcome_unknown",
            Self::NotPending => "not_pending",
            Self::Storage(_) => "storage",
        }
    }
}
impl From<rusqlite::Error> for BrokerError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
