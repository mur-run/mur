//! Re-pin MUR's own bundled MCP server after the supervisor refreshes it.
//!
//! `mur_common::exec::ensure_bundled_mcp_server` copies the `mur-mcp-server`
//! shipped beside the running `mur` into `~/.mur/mcp-servers/` whenever the two
//! differ — so every `mur` upgrade changes that binary underneath every profile
//! that pinned it at install time.
//!
//! Nothing re-pinned it, which left B0 rule 6 permanently reporting drift for
//! the one MCP MUR ships itself. That was survivable only because rule 6 never
//! actually refused a startup; now that it does (#791), an upgrade without a
//! re-pin would refuse to start the agent.
//!
//! Re-pinning here is not a weakening of the control. The binary was just
//! written by this runtime from its own installation — its trust anchor is
//! "same install as the runtime", not a hash recorded weeks ago. Anyone able to
//! swap it has already swapped `mur` itself. Third-party MCP entries, which are
//! the reason rule 6 exists, are never touched.

use std::path::Path;

/// Re-pin profile entries pointing at MUR's own bundled MCP server to the
/// binary now on disk at `bundled`.
///
/// Matches an entry when its command is that exact path, or the bare binary
/// name (`mur-mcp-server`, as written by a capability install). Returns `true`
/// when `profile.yaml` was rewritten.
pub fn repin_bundled_mcp(agent_home: &Path, bundled: &Path) -> anyhow::Result<bool> {
    let actual = crate::hooks::b0_helpers::binary_sha256(bundled)
        .map_err(|e| anyhow::anyhow!("hash {}: {e}", bundled.display()))?;

    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)?;

    let mut changed = false;
    for entry in &mut profile.mcp_servers {
        let command = Path::new(&entry.command);
        if command != bundled && command.file_name() != bundled.file_name() {
            continue;
        }
        if entry.binary_sha256.as_deref() == Some(actual.as_str()) {
            continue;
        }
        tracing::info!(
            mcp = %entry.name,
            from = entry.binary_sha256.as_deref().unwrap_or("<unpinned>"),
            to = %actual,
            "re-pinned bundled mcp-server after refresh",
        );
        entry.binary_sha256 = Some(actual.clone());
        changed = true;
    }
    if !changed {
        return Ok(false);
    }

    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&profile)?;
    // temp + rename: a torn profile.yaml would brick the agent.
    let tmp = profile_path.with_extension("tmp");
    std::fs::write(&tmp, new_yaml.as_bytes())?;
    std::fs::rename(&tmp, &profile_path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::AgentProfile;
    use mur_common::agent::McpServerEntry;

    fn entry(name: &str, command: &str, pin: Option<&str>) -> McpServerEntry {
        McpServerEntry {
            name: name.into(),
            command: command.into(),
            args: vec![],
            binary_sha256: pin.map(str::to_string),
            ..Default::default()
        }
    }

    /// Writes a profile whose entries are built from the bundled binary's real
    /// path, plus that binary on disk. Returns (agent_home, bundled_path).
    fn fixture(
        make_entries: impl FnOnce(&Path) -> Vec<McpServerEntry>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("mur-mcp-server");
        std::fs::write(&bundled, b"bundled binary v2").unwrap();

        let yaml = include_str!("../tests/fixtures/profile_minimal.yaml");
        let mut profile: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        profile.mcp_servers = make_entries(&bundled);
        std::fs::write(
            dir.path().join("profile.yaml"),
            serde_yaml_ng::to_string(&profile).unwrap(),
        )
        .unwrap();
        (dir, bundled)
    }

    fn reload(agent_home: &Path) -> AgentProfile {
        serde_yaml_ng::from_str(&std::fs::read_to_string(agent_home.join("profile.yaml")).unwrap())
            .unwrap()
    }

    #[test]
    fn repins_the_bundled_entry_and_leaves_third_party_pins_alone() {
        let third_party_pin = "a".repeat(64);
        let stale_pin = "b".repeat(64);
        let (dir, bundled) = fixture(|b| {
            vec![
                entry("media", &b.display().to_string(), Some(&"b".repeat(64))),
                entry("weather", "/opt/vendor/weather-mcp", Some(&"a".repeat(64))),
            ]
        });

        assert!(repin_bundled_mcp(dir.path(), &bundled).unwrap());

        let after = reload(dir.path());
        let expected = crate::hooks::b0_helpers::binary_sha256(&bundled).unwrap();
        assert_eq!(
            after.mcp_servers[0].binary_sha256.as_deref(),
            Some(expected.as_str()),
        );
        assert_ne!(expected, stale_pin, "fixture must actually be stale");
        assert_eq!(
            after.mcp_servers[1].binary_sha256.as_deref(),
            Some(third_party_pin.as_str()),
            "a third-party pin is exactly what rule 6 exists to protect",
        );
    }

    #[test]
    fn matches_a_bare_binary_name_as_written_by_capability_installs() {
        let (dir, bundled) = fixture(|_| vec![entry("media", "mur-mcp-server", None)]);

        assert!(repin_bundled_mcp(dir.path(), &bundled).unwrap());

        let expected = crate::hooks::b0_helpers::binary_sha256(&bundled).unwrap();
        assert_eq!(
            reload(dir.path()).mcp_servers[0].binary_sha256.as_deref(),
            Some(expected.as_str()),
        );
    }

    #[test]
    fn is_idempotent_and_does_not_rewrite_when_already_current() {
        let (dir, bundled) = fixture(|_| vec![entry("media", "mur-mcp-server", None)]);
        assert!(repin_bundled_mcp(dir.path(), &bundled).unwrap());

        let before = std::fs::read_to_string(dir.path().join("profile.yaml")).unwrap();
        assert!(
            !repin_bundled_mcp(dir.path(), &bundled).unwrap(),
            "second run has nothing to change"
        );
        assert_eq!(
            before,
            std::fs::read_to_string(dir.path().join("profile.yaml")).unwrap(),
            "an unchanged pin must not rewrite profile.yaml (updated_at would churn)",
        );
    }
}
