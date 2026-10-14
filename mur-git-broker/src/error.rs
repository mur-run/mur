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
    /// No enrollment record binds this (agent, repo, remote).
    #[error("not_enrolled")]
    NotEnrolled,
    /// The enrollment file could not be read or parsed. Fails closed: nothing is decided on it.
    #[error("enrollment_unavailable")]
    EnrollmentUnavailable,
    /// The credential file could not be used (missing, wrong mode or owner, outside `secrets/`,
    /// ...). The payload is a fixed reason token, never a path or file content. Retryable: the
    /// approval is not spent (credentials design §3).
    #[error("credential_unreadable: {0}")]
    CredentialUnreadable(&'static str),
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
            Self::NotEnrolled => "not_enrolled",
            Self::EnrollmentUnavailable => "enrollment_unavailable",
            Self::CredentialUnreadable(_) => "credential_unreadable",
            Self::Storage(_) => "storage",
        }
    }
}
impl From<rusqlite::Error> for BrokerError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
