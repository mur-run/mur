//! Character card schema (`.murcard.yaml`) — CCv3-compatible with a
//! `extensions.mur` namespace for mur-specific additions.
//!
//! Spec: `docs/superpowers/specs/2026-04-30-mur-agent-d2-onboarding-design.md`
//! §4.4.

pub mod extensions;
pub mod first_memory;
pub mod schema;
pub mod serde_round_trip;
