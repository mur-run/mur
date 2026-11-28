//! Which `HitlResponse.surface` values are a human answering (spec §7).
//!
//! An allowlist, not a deny list: a new machine surface (`reviewer`,
//! `shadow`, anything later) is excluded by default instead of by someone
//! remembering to add it. `auto` (a CLI session lane) and `unknown` (a writer
//! that named no surface) are not human.

/// Surfaces where a person pressed the key. `mcp` = an MCP client's
/// elicitation answer relayed by the shim.
pub const HUMAN_SURFACES: &[&str] = &["cli", "hub", "ios", "mcp"];

/// The surface tests write. Accepted only where the caller opts in under
/// its own `#[cfg(test)]`: `cfg(test)` here would apply to this crate's
/// tests only, never to a dependent crate's.
pub const TEST_SURFACE: &str = "test";

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
    }
}
