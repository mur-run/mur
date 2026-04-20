//! Sleep-time compact pipeline (Phase 2A, spec §4).
//!
//! Produces daily hybrid summaries: frontmatter + extractive spans +
//! abstractive narrative + macro expansion map. See
//! `docs/superpowers/specs/2026-04-20-mur-conversations-phase-2-design.md`.
#![allow(dead_code)] // public API wired progressively across Tasks 4-10.

pub mod abstractive;
pub mod chunker;
pub mod extractive;
pub mod macro_refs;
// Later tasks add: pub mod writer;
