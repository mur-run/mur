//! Which `HitlResponse.surface` values are a human answering (spec §7).
//!
//! An allowlist, not a deny list: a new machine surface (`reviewer`,
//! `shadow`, anything later) is excluded by default instead of by someone
//! remembering to add it. `auto` (a CLI session lane) and `unknown` (a writer
//! that named no surface) are not human.

/// Surfaces where a person pressed the key. `mcp` = an MCP client's
/// elicitation answer relayed by the shim.
pub const HUMAN_SURFACES: &[&str] = &[CLI_SURFACE, HUB_SURFACE, IOS_SURFACE, MCP_SURFACE];

/// The surface tests write. Accepted only where the caller opts in under
/// its own `#[cfg(test)]`: `cfg(test)` here would apply to this crate's
/// tests only, never to a dependent crate's.
pub const TEST_SURFACE: &str = "test";

/// The surface for an answer no person gave at the moment of asking: a
/// session lane / "don't ask again" grant replaying an earlier answer. Never
/// human, so it is never replayed again in its own right.
pub const AUTO_SURFACE: &str = "auto";

/// The Hub desktop app, when a person pressed Allow / Deny on a card.
pub const HUB_SURFACE: &str = "hub";

/// A terminal: murmur, `mur agent hitl`, `mur channel`, a review session.
pub const CLI_SURFACE: &str = "cli";

/// An MCP client's elicitation answer, relayed by the runtime's shim.
pub const MCP_SURFACE: &str = "mcp";

/// The iOS companion app, when a person approved or rejected on the phone.
pub const IOS_SURFACE: &str = "ios";

/// A standing policy grant settling a call without asking. Never human.
pub const POLICY_SURFACE: &str = "policy";

/// The responder named no surface. Recorded, never guessed, never human.
pub const UNKNOWN_SURFACE: &str = "unknown";

pub fn is_human_surface(surface: &str) -> bool {
    HUMAN_SURFACES.contains(&surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_surfaces_are_an_allowlist() {
        for s in ["cli", "hub", "ios", "mcp"] {
            assert!(is_human_surface(s), "{s}");
        }
        for s in [
            "reviewer", "shadow", "auto", "policy", "unknown", "", "CLI", "test",
        ] {
            assert!(!is_human_surface(s), "{s}");
        }
        assert!(!is_human_surface(AUTO_SURFACE));
        assert!(is_human_surface(HUB_SURFACE));
        assert!(is_human_surface(CLI_SURFACE));
        assert!(is_human_surface(MCP_SURFACE));
        assert!(is_human_surface(IOS_SURFACE));
        assert!(!is_human_surface(POLICY_SURFACE));
        assert!(!is_human_surface(UNKNOWN_SURFACE));
    }
}
