//! MUR Core v2 — Continuous learning for AI assistants.
//!
//! This library exposes the core API for pattern management,
//! retrieval, and evolution. Used by the `mur` CLI binary
//! and by MUR Commander (daemon).

pub mod a2a_dial;
pub mod agent_admin;
pub mod auth;
pub mod bridge_keychain;
pub mod capture;
pub mod character_card;
pub mod codebase;
pub mod daemon;
// `cmd` is shared with the binary (`main.rs`). When compiled as part of the
// library, most CLI dispatch fns are unreachable — they're only invoked by
// the binary's clap parser — so `dead_code` warnings here are expected.
// `agent_admin` calls into the small subset (perm/mcp/skill/prompt mutators
// + helpers like `load_profile_for_edit`).
#[doc(hidden)]
#[allow(dead_code)]
pub mod cmd;
pub mod community;
pub mod context_api;
pub mod conversations;
pub mod dashboard;
pub mod discovery;
pub mod evolve;
pub mod executor;
pub mod extract;
pub mod extract_llm;
pub mod federation;
pub mod gep;
pub mod inject;
pub mod interactive;
pub mod paths;
pub mod retrieve;
pub mod session;
pub mod skill_consolidate;
pub mod skill_gen;
pub mod skill_lifecycle;
pub mod skill_repair;
pub mod skill_stats;
pub mod sources;
pub mod store;
pub mod sync;
pub mod team;
pub mod update;
pub mod verify;
pub mod yaml_edit;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod server_agents;

pub use mur_common::config::Config;
pub use mur_common::event::MurEvent;
pub use mur_common::pattern::Pattern;
