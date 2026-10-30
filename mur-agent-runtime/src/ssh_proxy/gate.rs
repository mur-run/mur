//! The approval step between policy and the upstream (plan Task 3.2).
//!
//! The proxy only knows that a signature needs a human's yes; how that yes
//! is obtained (gate B's store, prompt, `hitl/pending`) lives in
//! `crate::hitl::ssh`, so this module does not depend on the HITL plumbing.

use std::sync::{Arc, OnceLock};

/// What the person approving is asked about. Labels come from the policy,
/// fingerprints from the verified `session-bind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignAsk {
    pub key_fp: String,
    pub key_label: Option<String>,
    pub host_key_fp: String,
    pub host_label: Option<String>,
}

/// The gate's answer. Only `Allow` lets a sign request reach the upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    /// A person said no, now or earlier (remembered).
    Deny,
    /// Nobody can be asked: the approval channel is not there. Never passes
    /// the request through.
    Unavailable(String),
    /// Asked, not answered in time. The request stays pending; an answer
    /// arriving later is remembered for the next sign request.
    Deferred,
}

#[async_trait::async_trait]
pub trait SignGate: Send + Sync {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome;
}

/// Why [`GateSlot`] refuses before it is filled.
pub const GATE_NOT_READY: &str = "the approval channel is not ready yet";

/// A gate filled in after the proxy starts. The proxy starts at the seal,
/// before the supervisor has its approval channel; until [`Self::set`] runs,
/// every request is `Unavailable`, never passed through.
#[derive(Default)]
pub struct GateSlot(OnceLock<Arc<dyn SignGate>>);

impl GateSlot {
    /// Fill the slot. The first gate wins; a second call returns `false` and
    /// changes nothing, so the gate cannot be swapped while running.
    pub fn set(&self, gate: Arc<dyn SignGate>) -> bool {
        self.0.set(gate).is_ok()
    }
}

#[async_trait::async_trait]
impl SignGate for GateSlot {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome {
        match self.0.get() {
            Some(g) => g.decide(ask).await,
            None => GateOutcome::Unavailable(GATE_NOT_READY.into()),
        }
    }
}
