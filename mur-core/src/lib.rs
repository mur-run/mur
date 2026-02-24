//! MUR Core v2 — Continuous learning for AI assistants.
//!
//! This library exposes the core API for pattern management,
//! retrieval, and evolution. Used by the `mur` CLI binary
//! and by MUR Commander (daemon).

pub mod capture;
pub mod store;
pub mod retrieve;
pub mod evolve;
pub mod inject;
pub mod migrate;

pub use mur_common::pattern::Pattern;
pub use mur_common::config::Config;
pub use mur_common::event::MurEvent;
