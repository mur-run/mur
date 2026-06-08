/// Decision returned by the Hub (or any HITL responder) for a pending approval.
#[derive(Debug, Clone)]
pub struct HitlDecision {
    pub allow: bool,
    pub reason: Option<String>,
}
