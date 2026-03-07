//! MUR Core v2 — Continuous learning for AI assistants.
//!
//! This library exposes the core API for pattern management,
//! retrieval, and evolution. Used by the `mur` CLI binary
//! and by MUR Commander (daemon).

pub mod auth;
pub mod capture;
pub mod community;
pub mod context_api;
pub mod dashboard;
pub mod evolve;
pub mod gep;
pub mod inject;
pub mod interactive;
pub mod llm;
pub mod retrieve;
pub mod server;
pub mod session;
pub mod store;
pub mod team;

pub use mur_common::config::Config;
pub use mur_common::event::MurEvent;
pub use mur_common::pattern::Pattern;
