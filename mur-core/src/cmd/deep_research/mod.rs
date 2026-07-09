//! `mur deep-research` command module.
//!
//! Provisions restricted worker agents (Task 7 of the MUR-native
//! deep-research plan) that each mount the `research-gateway` MCP server
//! (`mur-research-gateway`, Tasks 1-6). Workers hold no outbound egress of
//! their own — only the gateway process does — and the per-server MCP
//! egress grant is a separate, explicit-consent step (Task 8).

pub mod provision;
