//! Conversations archive — local-only, cross-source record of every AI
//! coding-assistant and chat-platform interaction.
//!
//! See `docs/superpowers/specs/2026-04-19-mur-conversations-design.md`.

pub mod audit;
pub mod blob;
pub mod index;
pub mod ingest;
pub mod paths;
pub mod store;
