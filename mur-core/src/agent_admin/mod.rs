//! Library API for per-agent admin operations.
//!
//! This module is the public surface for callers other than the `mur` CLI
//! (e.g. Tauri command handlers in `mur-agent-gui`). It exposes the same
//! operations that `mur agent <verb>` performs, but as programmatic
//! functions: mutators return `Result<()>` and printing-free; queries
//! return typed values that the caller can serialise (JSON for IPC, YAML
//! for stdout, etc.).
//!
//! The CLI dispatch in `crate::cmd::agent` continues to be the canonical
//! implementation for mutators (most `cmd_*` functions are already
//! print-free internally — they just mutate `~/.mur/agents/<name>/profile.yaml`).
//! This module provides a clean naming + typed-query layer on top.
//!
//! See `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md` § 4.6.

pub mod error;
pub mod lifecycle;
pub mod mcp;
pub mod observability;
pub mod perm;
pub mod prompt;
pub mod skill;

pub use error::{AgentAdminError, AgentAdminResult};
