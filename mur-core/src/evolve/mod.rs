#[allow(dead_code)] // Commander bridge — used by mur-commander crate
pub mod commander_bridge;
pub mod compose;
pub mod consolidate;
pub mod cooccurrence;
pub mod decay;
pub mod decompose;
pub mod feedback;
pub mod lifecycle;
pub mod linker;
pub mod maturity;

use mur_common::pattern::Pattern;

use self::commander_bridge::{CommanderBridge, WorkflowPreview};

/// After pattern evolution, check if any patterns are automatable and suggest
/// Commander workflows for them.
///
/// Returns previews for patterns that are candidates. Callers decide whether
/// to present them to the user or auto-save.
#[allow(dead_code)] // Used by mur-commander
pub fn suggest_commander_workflows(
    bridge: &CommanderBridge,
    patterns: &[Pattern],
) -> Vec<WorkflowPreview> {
    let candidates = bridge.detect_workflow_candidates(patterns);
    candidates
        .into_iter()
        .filter_map(|c| {
            let pattern = patterns.iter().find(|p| p.name == c.pattern_name)?;
            // Skip if workflow already exists on disk
            if commander_bridge::workflow_exists(&bridge.config.workflows_dir, &pattern.name) {
                return None;
            }
            bridge.suggest_workflow(pattern).ok().flatten()
        })
        .collect()
}
