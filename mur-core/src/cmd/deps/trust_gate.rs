//! Pure decision for whether to offer a trusted-publisher recipe install at
//! import. No I/O — the caller supplies trust classification + detect status.

use mur_common::deps::registry::{CuratedRecipe, is_curated};
use mur_common::deps::{DepStatus, ProgramDep};
use mur_common::skill::publisher_trust::PublisherTrust;

#[derive(Debug)]
pub enum GateDecision {
    /// Offer to install this (already platform-resolved) recipe.
    Offer(CuratedRecipe),
    /// Publisher not in the keyring — detect-and-guide only.
    SkipUntrusted,
    /// Publisher revoked — refuse.
    SkipRevoked,
    /// Name is a MUR-curated key — curated wins, handled by install-deps.
    SkipCurated,
    /// Already present — never reinstall.
    SkipPresent,
    /// No author recipe declared.
    SkipNoRecipe,
    /// Recipe declared but no entry for the current platform.
    SkipNoPlatformRecipe,
}

/// Decide the gate for one dep. Precedence: present → curated → no-recipe →
/// no-platform → trust (revoked/unknown/trusted).
#[allow(dead_code)]
pub fn decide(
    dep: &ProgramDep,
    trust: PublisherTrust,
    status: DepStatus,
    platform: &str,
) -> GateDecision {
    if status == DepStatus::Present {
        return GateDecision::SkipPresent;
    }
    let key = dep.registry.as_deref().unwrap_or(&dep.name);
    if is_curated(key) {
        return GateDecision::SkipCurated;
    }
    let Some(recipe) = &dep.recipe else {
        return GateDecision::SkipNoRecipe;
    };
    let Some(curated) = recipe.for_platform(platform) else {
        return GateDecision::SkipNoPlatformRecipe;
    };
    match trust {
        PublisherTrust::Revoked => GateDecision::SkipRevoked,
        PublisherTrust::Unknown => GateDecision::SkipUntrusted,
        PublisherTrust::Trusted => GateDecision::Offer(curated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::deps::{DepStatus, DetectMethod, PlatformRecipe, ProgramDep, ProgramRecipe};
    use mur_common::skill::publisher_trust::PublisherTrust;
    use std::collections::BTreeMap;

    fn dep_with_recipe(name: &str, plat: &str) -> ProgramDep {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            plat.to_string(),
            PlatformRecipe {
                url: "u".into(),
                sha256: "s".into(),
                install_to: Some("aura/x".into()),
                executable: true,
                archive: None,
            },
        );
        ProgramDep {
            name: name.into(),
            detect: DetectMethod::Command {
                command: name.into(),
            },
            reason: "r".into(),
            hint: None,
            registry: None,
            recipe: Some(ProgramRecipe { platforms }),
        }
    }

    #[test]
    fn decision_table() {
        let d = dep_with_recipe("some-tool", "aarch64-macos");
        // Trusted + missing + has-platform-recipe + not-curated → Offer
        assert!(matches!(
            decide(
                &d,
                PublisherTrust::Trusted,
                DepStatus::Missing,
                "aarch64-macos"
            ),
            GateDecision::Offer(_)
        ));
        // Unknown publisher → SkipUntrusted
        assert!(matches!(
            decide(
                &d,
                PublisherTrust::Unknown,
                DepStatus::Missing,
                "aarch64-macos"
            ),
            GateDecision::SkipUntrusted
        ));
        // Revoked → SkipRevoked (even though Trusted-eligible otherwise)
        assert!(matches!(
            decide(
                &d,
                PublisherTrust::Revoked,
                DepStatus::Missing,
                "aarch64-macos"
            ),
            GateDecision::SkipRevoked
        ));
        // Present → SkipPresent (never reinstall)
        assert!(matches!(
            decide(
                &d,
                PublisherTrust::Trusted,
                DepStatus::Present,
                "aarch64-macos"
            ),
            GateDecision::SkipPresent
        ));
        // No recipe entry for this platform → SkipNoPlatformRecipe
        assert!(matches!(
            decide(
                &d,
                PublisherTrust::Trusted,
                DepStatus::Missing,
                "x86_64-windows"
            ),
            GateDecision::SkipNoPlatformRecipe
        ));
        // No recipe at all → SkipNoRecipe
        let mut d_no = d.clone();
        d_no.recipe = None;
        assert!(matches!(
            decide(
                &d_no,
                PublisherTrust::Trusted,
                DepStatus::Missing,
                "aarch64-macos"
            ),
            GateDecision::SkipNoRecipe
        ));
        // Curated name → SkipCurated (curated wins). "obscura" is a Phase-1 curated key.
        let mut d_cur = dep_with_recipe("obscura", "aarch64-macos");
        d_cur.registry = None; // name is the key
        assert!(matches!(
            decide(
                &d_cur,
                PublisherTrust::Trusted,
                DepStatus::Missing,
                "aarch64-macos"
            ),
            GateDecision::SkipCurated
        ));
    }
}
