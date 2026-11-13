//! The single implementation behind the CLI, murmur and Hub (SSH setup spec,
//! `mur-core` — `cmd/agent/ssh/setup/`): `plan`, `apply_and_restart`, and
//! `status` (in `super::status`).

pub mod apply;
pub mod life;
pub mod plan;
pub mod preapprove;
pub mod probe;
pub mod verdict;

pub use apply::{Applied, Approval, apply_and_restart};
pub use plan::{SetupPlan, SpawnChange, plan};
pub use verdict::Verdict;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
