//! Track C3 channel B — global hotkey capture.
//!
//! Bound at app startup via [`tauri-plugin-global-shortcut`]. When the
//! user hits the bound combo, we read the system clipboard
//! (`tauri-plugin-clipboard-manager`), synthesize a [`SharePayload`],
//! and hand it to the [`SendIngestor`].
//!
//! `lib.rs::setup` wiring is deferred to a follow-up so we can iterate
//! on the combo + clipboard logic in isolation; the [`crate::test_harness::MockApp`]
//! drives the same parse → ingest path used by production.
//!
//! [`SendIngestor`]: super::SendIngestor

/// Default hotkey combo for an agent.
///
/// Single-agent installs land on `Cmd+Shift+M`; the per-agent suffix
/// (the first letter of the slug, uppercased) only matters when more
/// than one mur agent is installed. Two agents whose slugs share a
/// first letter still collide — see
/// [`resolve_combo`] for the user-override escape hatch.
///
/// `slug` is expected to be the kebab-case agent name produced by
/// `mur-core::cmd::agent_export_gui::sanitize_for_bundle_id`. An
/// empty slug falls back to `A` so the combo stays valid.
pub fn default_combo_for(slug: &str) -> String {
    let first = slug
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('A');
    format!("CommandOrControl+Shift+M+{first}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_default_combo_for_slug() {
        assert_eq!(default_combo_for("coach"), "CommandOrControl+Shift+M+C");
        assert_eq!(default_combo_for("draft"), "CommandOrControl+Shift+M+D");
        assert_eq!(default_combo_for("mur-bot"), "CommandOrControl+Shift+M+M");
    }

    #[test]
    fn unit_default_combo_empty_slug_falls_back_to_a() {
        assert_eq!(default_combo_for(""), "CommandOrControl+Shift+M+A");
    }
}
