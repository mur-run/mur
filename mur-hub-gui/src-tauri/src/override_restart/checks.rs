//! The Hub-side checks that run before the orchestrator (Hub spec §Command,
//! checks 1–3). Each fails closed with nothing written.

use std::path::Path;

use mur_common::agent::OverrideFlags;
use mur_common::validate_agent_name;
use mur_core::a2a_dial::canonicalize_agent_name;
use mur_core::cmd::agent::NOTHING_CHANGED;

/// The only window allowed to ask. App commands have no per-window ACL, so
/// this label check is the window boundary.
pub const DASHBOARD_LABEL: &str = "dashboard";

/// Check 1.
pub fn check_window(label: &str) -> Result<(), String> {
    if label == DASHBOARD_LABEL {
        Ok(())
    } else {
        Err(format!(
            "startup overrides can only be requested from the MUR dashboard; {NOTHING_CHANGED}"
        ))
    }
}

/// Check 2.
pub fn check_flags(flags: OverrideFlags) -> Result<(), String> {
    if flags.cli_names().is_empty() {
        Err(format!("no override flag was selected; {NOTHING_CHANGED}"))
    } else {
        Ok(())
    }
}

/// Check 3. Validation runs before any path is built: canonicalization
/// returns `../x` unchanged when a matching `profile.yaml` exists.
///
/// The directory entry itself must spell `name` exactly. On a
/// case-insensitive filesystem (the macOS default) `agents/Foo/profile.yaml`
/// is a file when the directory is `foo`, and `canonicalize_agent_name`
/// returns `Foo` unchanged from its exact-match branch, so comparing its
/// result alone lets a case variant through.
pub fn check_name(home: &Path, name: &str) -> Result<(), String> {
    validate_agent_name(name).map_err(|e| format!("{e}; {NOTHING_CHANGED}"))?;
    let agents = home.join("agents");
    let exact_entry = std::fs::read_dir(&agents)
        .map(|it| it.flatten().any(|e| e.file_name() == name))
        .unwrap_or(false);
    if !exact_entry
        || canonicalize_agent_name(home, name) != name
        || !agents.join(name).join("profile.yaml").is_file()
    {
        return Err(format!("agent '{name}' not found; {NOTHING_CHANGED}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
