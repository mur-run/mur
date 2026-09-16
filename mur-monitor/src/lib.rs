//! Durable monitor: keep checking asynchronous work until it is settled.
//! Sits below `mur-core` so an agent runtime can register a monitor without
//! pulling the vector store in. Adapters that need `mur-core` live there.

// Lets this crate's own unit tests address items through the same
// `mur_monitor::...` path an external consumer would use (e.g.
// `action::mod`'s `mur_monitor::spec::KNOWN_ACTIONS`). Without this, a unit
// test referring to the crate by its own name fails to compile — Cargo
// only registers a lib crate under its own name for crates that depend on
// it (integration tests, doctests), never for the crate's own unit tests.
// Gated to `cfg(test)` so it has no effect on the real build.
#[cfg(test)]
extern crate self as mur_monitor;

pub mod action;
pub mod adapter;
pub mod backoff;
pub mod deadline;
pub mod notify;
pub mod scheduler;
pub mod spec;
pub mod state;
pub mod store;
