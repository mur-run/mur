//! B0 rule 6 (M9.3) for one MCP entry — the body of the per-entry loop in
//! [`super::verify_mcp_supply_chain`], split out so a later reload path can
//! run the exact same check on a single entry.

use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;

/// Install-time pin verification for one enabled MCP entry. Same contract as
/// the caller: a returned `Err` refuses startup; soft failures only warn.
pub(super) fn verify_entry_pin(
    entry: &McpServerEntry,
    profile: &AgentProfile,
) -> Result<(), String> {
    // A vendored package is the one interpreter-launched shape that CAN be
    // verified: MUR installed it into a directory it owns, so the lockfile
    // recorded at approval still describes the tree unless something
    // changed it. Checked before the interpreter skip below, which would
    // otherwise wave through the very case made verifiable on purpose.
    if let Some(pkg) = &entry.package {
        let lock = pkg.lockfile_path();
        match crate::hooks::b0_helpers::binary_sha256(&lock) {
            Ok(actual) if actual.eq_ignore_ascii_case(&pkg.lockfile_sha256) => {
                tracing::debug!(mcp = %entry.name, "B0 rule 6: vendored package verified");
            }
            Ok(_) => {
                return Err(format!(
                    "B0 rule 6: vendored MCP `{}` ({}@{}) no longer matches the tree \
                     installed at approval time — re-run `mur agent mcp vendor {} {}` \
                     to reinstall and re-approve, or `mur agent mcp remove {} {}` \
                     to uninstall.",
                    entry.name,
                    pkg.name,
                    pkg.version,
                    profile.name,
                    entry.name,
                    profile.name,
                    entry.name,
                ));
            }
            Err(reason) => {
                // Install gone or unreadable. Same call as a missing binary
                // below: far more likely "the user deleted it" than an
                // attack, and refusing to boot would strand the agent with
                // no way back in.
                tracing::warn!(
                    mcp = %entry.name,
                    reason = %reason,
                    "B0 rule 6: vendored package lockfile unreadable; continuing",
                );
            }
        }
        return Ok(());
    }
    let Some(expected) = &entry.binary_sha256 else {
        // Pre-M9 entry — skip silently; the cookbook documents
        // `mur agent mcp pin <name> <server-id>` as the migration verb.
        return Ok(());
    };
    // An interpreter-launched server (`npx @scope/pkg`, `python -m …`) pins
    // the *interpreter*, not the server. Refusing startup on that hash
    // would brick the agent on any unrelated Node/Python upgrade while
    // still not covering the code it runs, so the pin is reported as
    // unprotected rather than enforced. Package-level pinning is the real
    // fix and a different mechanism.
    if mur_common::exec::is_interpreter_command(&entry.command) {
        tracing::warn!(
            mcp = %entry.name,
            command = %entry.command,
            "B0 rule 6: interpreter-launched MCP — binary pin does not cover the server code, \
             not enforced (see `mur doctor`)",
        );
        return Ok(());
    }
    // Resolve against the SAME augmented PATH the spawn uses
    // (`protocol/mcp_client.rs`, `supervisor_runner/prepare.rs`), so the file
    // hashed here is provably the file `Command::new` will exec. A bare
    // `node`/`npx` opened verbatim is a CWD-relative path that doesn't
    // exist, which previously soft-failed and skipped the pin entirely.
    //
    // Resolving against the ambient PATH instead was not merely cosmetic.
    // `augmented_path_var` appends `~/.local/bin` — where an installed MUR
    // lives since #935 — and launchd/systemd hand the runtime a PATH
    // without it. So a first-party binary found only there resolved for the
    // spawn but not for this check: verification soft-failed with "command
    // not resolvable, continuing" and the binary then ran entirely
    // unpinned. The two passes must consult one PATH.
    //
    // And one command: the spawn resolves `entry.command` whole (args live
    // in `entry.args`). Taking its first word truncated a path with a
    // space (`~/Library/Application Support/…`) to one that does not
    // exist, which soft-failed here and ran the server unpinned.
    //
    // And the SAME ordering: under every fenced spawn mode the spawn uses
    // the seal-first `mcp_child_path`, not the bare augmented PATH. With
    // two same-named binaries, hashing the inherited-PATH copy would
    // approve one file while the runtime execs the other, unchecked.
    let path = match mur_common::exec::resolve_mcp_launch_in(
        &crate::sandbox::search_dirs::mcp_child_path(profile.entitlements.processes.spawn.mode),
        &entry.command,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                mcp = %entry.name,
                error = %e,
                "B0 rule 6: pin verify soft-failed (command not resolvable); continuing",
            );
            return Ok(());
        }
    };
    match crate::hooks::b0_helpers::verify_mcp_binary_hash(expected, &path) {
        Ok(()) => tracing::debug!(mcp = %entry.name, "B0 rule 6: pin verified"),
        Err(crate::hooks::b0_helpers::PinDriftReason::BinaryDrift { .. }) => {
            return Err(format!(
                "B0 rule 6: MCP `{}` changed since install — \
                 run `mur agent mcp inspect {} --server {}` to review the \
                 drift and `mur agent mcp pin {} {}` to re-approve, \
                 or `mur agent mcp remove {} {}` to uninstall.",
                entry.name,
                profile.name,
                entry.name,
                profile.name,
                entry.name,
                profile.name,
                entry.name,
            ));
        }
        Err(soft @ crate::hooks::b0_helpers::PinDriftReason::BinaryMissing { .. })
        | Err(soft @ crate::hooks::b0_helpers::PinDriftReason::BinaryReadError { .. }) => {
            tracing::warn!(
                mcp = %entry.name,
                reason = %soft,
                "B0 rule 6: pin verify soft-failed; continuing",
            );
        }
    }
    Ok(())
}
