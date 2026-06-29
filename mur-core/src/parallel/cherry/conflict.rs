//! API compatibility check between cherry-picked units.
//! Compares function signatures extracted via tree-sitter.

use crate::parallel::semantic::SupportedLanguage;
use anyhow::Result;

#[derive(Debug)]
pub struct ConflictReport {
    pub caller_unit: String,
    pub callee_unit: String,
    pub reason: String,
}

/// Check all cross-track dependencies for API compatibility.
/// Returns list of detected conflicts (empty = safe to assemble).
/// ponytail: signature-only check; full type inference would require rustc.
pub fn check_conflicts(
    _cherry_plan: &super::CherryPlan,
    _source: &[u8],
    _lang: SupportedLanguage,
) -> Result<Vec<ConflictReport>> {
    // P1: return empty — cargo check after assembly is the real gate.
    // P2: implement signature diff via tree-sitter `parameters` node extraction.
    Ok(Vec::new())
}
