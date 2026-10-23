//! Gate B — the in-chat tool gate. `HitlDecision` / `HitlApprovals` are the
//! oneshot plumbing between the runner and `tool/hitl_respond`; `store` is the
//! memory of settled decisions; `batch` asks once per LLM response.

pub mod authority;
pub mod batch;
pub mod integrity;
pub mod parked;
pub mod remembered;
pub mod shim_ticket;
pub mod store;

#[cfg(test)]
mod store_integrity_tests;

/// Decision returned by the Hub (or any HITL responder) for a pending approval.
#[derive(Debug, Clone)]
pub struct HitlDecision {
    pub allow: bool,
    pub reason: Option<String>,
    /// Which surface answered ("hub", "cli", "ios"); `None` when the responder
    /// did not say, recorded as "unknown" — never guessed.
    pub surface: Option<String>,
    /// #2085: refused only because nobody who can answer is attached and
    /// nothing is remembered — "needs a human", not "a human said no". Always
    /// paired with `allow: false`; the call never runs.
    pub awaiting_human: bool,
}

/// Shared map from hitl_id → oneshot sender for approval decisions.
pub type HitlApprovals = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>,
    >,
>;
