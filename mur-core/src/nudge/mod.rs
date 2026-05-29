//! Workflow nudge engine: turn emergence-mined recurring behavior into
//! actionable "save this as a workflow?" prompts (surface-agnostic).
pub mod candidate;
pub mod emitter;
pub mod ledger;

#[allow(unused_imports)]
pub use candidate::{CandidateSource, EmergenceSource, WorkflowCandidate};
#[allow(unused_imports)]
pub use emitter::{NudgeDecision, NudgeEmitter};
#[allow(unused_imports)]
pub use ledger::{NudgeLedger, NudgeRecord, NudgeState};
