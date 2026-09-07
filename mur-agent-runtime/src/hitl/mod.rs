/// Decision returned by the Hub (or any HITL responder) for a pending approval.
#[derive(Debug, Clone)]
pub struct HitlDecision {
    pub allow: bool,
    pub reason: Option<String>,
}

/// Shared map from hitl_id → oneshot sender for approval decisions.
pub type HitlApprovals = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>,
    >,
>;
