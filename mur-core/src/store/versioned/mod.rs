//! Git-backed versioned stores (E1).
//!
//! Only the agents layer remains: [`agent::VersionedAgentStore`] wraps
//! `~/.mur/agents/` as a git repository with an O(1) version index. The
//! knowledge-layer `VersionedYamlStore` that versioned `~/.mur/patterns/`
//! was retired with the Pattern type (Tier C3) — skills carry their own
//! history via the registry / `skill.yaml` revision fields.
//!
//! ADR-0001 compliance (still enforced by `git_ops`):
//!   FIN-1 — `commit_paths` stages only explicit paths (no `add_all`)
//!   FIN-2 — version derivation O(1) via index lookup

pub mod agent;
mod agent_index;
mod git_ops;
