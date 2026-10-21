//! Bring a service descriptor written before #2015 up to the refusal-aware
//! template.
//!
//! `install-service` now writes a launchd plist whose `KeepAlive` is gated on
//! the refusal marker, and a systemd unit with `RestartPreventExitStatus=`.
//! Neither reaches a descriptor that is already installed: launchd keeps the
//! `KeepAlive=true` it read at load time, so a refusing runtime is respawned
//! forever no matter what the new runtime writes or exits with.
//!
//! The upgrade itself cannot fix that — `mur update` runs the *old* CLI's
//! post-upgrade code — so the new CLI does it on the paths a user takes next:
//! `mur agent start` migrates before it kicks the service (the user is
//! starting the agent anyway, and a reload is what applies the new policy),
//! and `mur agent status` / later upgrades name every descriptor still
//! lacking the guard.
//!
//! Nothing migrates unasked: re-installing reloads the job, and `RunAtLoad`
//! would bring back an agent the user had deliberately stopped.

use std::path::Path;

use anyhow::Result;

/// Whether the descriptor text already stops respawning a refused start.
///
/// Keyed on the guard itself, not on a whole-template comparison: the rest of
/// the descriptor (PATH, log paths) legitimately differs per machine and per
/// install, and re-installing over those differences is not this module's call.
pub(super) fn has_refusal_guard(descriptor: &str, marker: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let marker = marker.display().to_string();
        descriptor.contains("<key>PathState</key>")
            && descriptor.contains(&format!("<key>{marker}</key>"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = marker;
        let want = format!(
            "RestartPreventExitStatus={}",
            mur_common::start_refusal::EXIT_CODE
        );
        descriptor.lines().any(|l| l.trim() == want)
    }
}

/// `true` when `name` has an installed descriptor that predates the guard.
///
/// An unreadable descriptor answers `false`: "could not tell" must not turn
/// into a rewrite of a file we were not able to read.
pub(crate) fn needs_migration(name: &str, agent_home: &Path) -> bool {
    let Some(path) = super::service::installed_service(name) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    !has_refusal_guard(&text, &mur_common::start_refusal::marker_path(agent_home))
}

/// Re-install `name`'s service when its descriptor lacks the guard. Returns
/// whether it did. Re-installing reloads the job, which is the only way
/// launchd/systemd pick up the new restart policy.
pub(crate) fn migrate(name: &str, agent_home: &Path) -> Result<bool> {
    if !needs_migration(name, agent_home) {
        return Ok(false);
    }
    println!("updating '{name}''s service so a refused start is not respawned (#2015)…");
    super::service::cmd_install_service(name, false)?;
    Ok(true)
}

/// Agents under `agents_dir` whose installed descriptor lacks the guard —
/// for the post-upgrade checklist, which reports them rather than reloading.
pub(crate) fn unguarded(agents_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(agents_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .filter(|n| needs_migration(n, &agents_dir.join(n)))
        .collect();
    names.sort();
    names
}

/// The `status` line for a descriptor that still respawns a refused start.
pub(crate) fn status_hint(name: &str) -> String {
    format!(
        "   Service: installed before refusal handling — a refused start is respawned forever; \
         run `mur agent install-service {name}` (or `mur agent start {name}`) to update it"
    )
}

#[cfg(test)]
#[path = "service_migrate_tests.rs"]
mod tests;
