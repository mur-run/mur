//! Binary attestation mount helper — shared across all CLI spawn sites.
//!
//! Every path that spawns the agent runtime must verify the binary's
//! Developer ID signature first. Dev builds verify nothing but still
//! resolve, so a broken target always errors.

use std::path::Path;

use anyhow::{Context, Result};

/// Resolve a runtime path (canonicalizing through symlinks and the
/// /var -> /private/var redirect) and verify its Developer ID signature.
/// Dev builds verify nothing but still resolve, so a broken target always
/// errors.
///
/// When verification fails, the error carries the spec's canonical guidance
/// so every CLI-side mount site surfaces the same fix message.
pub(crate) fn verify_runtime_at(path: &Path) -> Result<()> {
    let real = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    mur_common::binary_attestation::verify_runtime_signature(&real).map_err(|e| {
        anyhow::anyhow!(
            "{e} — the runtime binary may have been swapped (launch-chain \
             protection covers writes, attestation covers swaps). Fix: mur \
             update --restart-agents, or reinstall MUR."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn verify_runtime_at_surfaces_resolution_errors() {
        // Even in a dev build (where verification is a no-op), a target that
        // cannot be resolved must fail — the mount never spawns blind.
        let err = verify_runtime_at(Path::new("/nonexistent/mur_agent_nope")).unwrap_err();
        assert!(
            err.to_string().contains("/nonexistent/mur_agent_nope"),
            "{err}"
        );
    }
}
