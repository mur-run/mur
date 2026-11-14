//! The single implementation behind the CLI, murmur and Hub (SSH setup spec,
//! `mur-core` — `cmd/agent/ssh/setup/`): `plan`, `apply_and_restart`, and
//! `status` (in `super::status`).

pub mod apply;
pub mod choices;
pub mod front;
pub mod life;
pub mod pending;
pub mod plan;
pub mod preapprove;
pub mod probe;
pub mod report;
pub mod verdict;

#[allow(unused_imports)] // Hub-only re-export.
pub use super::resolve::ScannedKey;
pub use apply::{Applied, Approval, apply_and_restart};
pub use plan::{SetupPlan, SpawnChange, plan};
pub use verdict::Verdict;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pending_tests.rs"]
mod pending_tests;

#[cfg(test)]
#[path = "choices_tests.rs"]
mod choices_tests;
