//! Workflow nudge engine: turn harvest-proposal candidates into actionable
//! "save this as a workflow?" prompts (surface-agnostic).
pub mod candidate;
pub mod companion;
pub mod emitter;
pub mod harvest_source;
pub mod ledger;

#[allow(unused_imports)]
pub use candidate::{CandidateSource, WorkflowCandidate};
#[allow(unused_imports)]
pub use emitter::{NudgeDecision, NudgeEmitter};
#[allow(unused_imports)]
pub use harvest_source::HarvestProposalSource;
#[allow(unused_imports)]
pub use ledger::{NudgeLedger, NudgeRecord, NudgeState};
