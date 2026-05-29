//! Workflow nudge engine: turn emergence-mined recurring behavior into
//! actionable "save this as a workflow?" prompts (surface-agnostic).
pub mod candidate;
pub mod ledger;
pub mod emitter;

pub use candidate::{CandidateSource, EmergenceSource, WorkflowCandidate};
pub use emitter::{NudgeDecision, NudgeEmitter};
pub use ledger::{NudgeLedger, NudgeRecord, NudgeState};
