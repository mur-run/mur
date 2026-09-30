//! Store-level entitlement pin bookkeeping (#712).
//!
//! Lives in `store` so both the CLI profile writers (`cmd::agent`) and the
//! versioned store (`store::versioned::agent::rollback_profile`) share one
//! implementation without the store layer depending on `cmd`.

use std::path::Path;

/// Move the #712 entitlement pin to what a trusted writer just saved — only
/// from a trusted state (see `entitlements_pin::advance_pin`). Best-effort: a
/// pin that could not advance makes the next start refuse with a `reseal` hint,
/// which is fail-closed, so the save itself still succeeds.
pub fn advance_entitlements_pin(
    profile_path: &Path,
    prior: Option<&mur_common::agent::Entitlements>,
    new: &mur_common::agent::Entitlements,
) {
    let Some(agent_dir) = profile_path.parent() else {
        return;
    };
    let (Some(agents_root), Some(name)) = (
        agent_dir.parent(),
        agent_dir.file_name().and_then(|n| n.to_str()),
    ) else {
        return;
    };
    let Some(mur_home) = agents_root.parent() else {
        return;
    };
    if agents_root.file_name().and_then(|n| n.to_str()) != Some("agents") {
        return;
    }
    match mur_common::entitlements_pin::advance_pin(mur_home, name, prior, new) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "warning: {name}'s entitlements had changed outside MUR before this save; \
             the agent will refuse to start until you review them and run \
             `mur agent perm reseal {name}`"
        ),
        Err(e) => tracing::warn!(agent = name, error = %e, "entitlement pin not updated"),
    }
}
