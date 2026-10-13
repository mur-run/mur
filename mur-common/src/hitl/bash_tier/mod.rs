//! Static risk tiering for agent tool calls — chiefly `bash` command lines.
//!
//! Moved here from `mur-core/src/cmd/agent/cli/` (#1814) so the agent
//! runtime, which must not depend on `mur-core`, can tier a `bash` call with
//! the same classifier the CLI's chat gate uses. Pure code movement: the
//! submodules are unchanged apart from visibility and import paths.
//!
//! - [`bash_class`] — the audited read-only head list.
//! - [`dest`] — tokenizer and per-segment destination scope.
//! - [`tool_tier`] — the [`crate::hitl::RiskTier`] of one tool call.

pub mod bash_class;
pub mod dest;
pub mod tool_tier;
