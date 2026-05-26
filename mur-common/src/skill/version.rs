//! Schema version anchor for skill manifests.
//!
//! Versions are additive-only. Existing fields never change semantics;
//! new optional fields bump the minor version.

/// Current schema version written by `mur skill create` / `mur skill publish`.
pub const SKILL_MANIFEST_SCHEMA_VERSION: &str = "2.1";

/// Accepted schema versions. v2.0 manifests without `mcp_requirements` default
/// to empty — behaviour unchanged.
pub fn is_supported(version: &str) -> bool {
    matches!(version, "2.0" | "2.1")
}
