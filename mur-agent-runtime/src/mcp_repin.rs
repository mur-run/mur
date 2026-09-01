//! Re-pin the MCP servers MUR ships itself, which every `mur` upgrade replaces.
//!
//! Two shapes, one trust anchor.
//!
//! **The bundled server.** `mur_common::exec::ensure_bundled_mcp_server` copies
//! the `mur-mcp-server` shipped beside the running `mur` into
//! `~/.mur/mcp-servers/` whenever the two differ — so every `mur` upgrade
//! changes that binary underneath every profile that pinned it at install time.
//!
//! **Sibling first-party servers.** `mur-research-gateway` (what
//! `mur deep-research setup` installs) is shipped in the same release, lands in
//! the same install directory, and is replaced by the same upgrade — but is
//! pinned by a bare command name like any third-party entry. Nothing exempted
//! it, so after #791 made rule 6 actually refuse startup, one `mur` upgrade
//! left every deep-research worker crash-looping at boot with `B0 rule 6: MCP
//! `research-gateway` changed since install`. launchd/systemd restarts it, it
//! fail-closes again, and `mur agent status` just says `stopped`.
//!
//! Re-pinning is not a weakening of the control. The anchor is "same install as
//! this runtime", not a hash recorded weeks ago: the bundled copy was written
//! moments ago by this runtime from its own installation, and a sibling is a
//! file in the very directory this runtime's own executable lives in. Anyone
//! able to swap either has already swapped the runtime itself.
//!
//! The anchor is deliberately a *property*, not a list of names — a list would
//! go stale the day MUR ships another server. Both halves of it are load-bearing
//! and neither is sufficient alone:
//!
//! * **Same directory as the running runtime.** A `mur-` prefixed binary found
//!   anywhere else on PATH is a stranger wearing MUR's name and stays enforced.
//! * **MUR's own `mur-` name prefix.** Without it, an unrelated third-party MCP
//!   binary the user happens to keep in `~/.local/bin` — a crowded directory on
//!   a normal machine, unlike a Homebrew Cellar — would silently stop being
//!   pin-enforced. That is coverage rule 6 exists to provide.
//!
//! Everything else is untouched: third-party entries, interpreter-launched
//! entries, and vendored packages.

use std::path::{Path, PathBuf};

/// The name prefix MUR's own shipped binaries carry (`mur-mcp-server`,
/// `mur-research-gateway`, …). Half of the first-party test above; see the
/// module docs for why matching on the directory alone is not enough.
const MUR_BINARY_PREFIX: &str = "mur-";

/// The first-party binary `entry` should be re-pinned to, or `None` when the
/// entry is third-party and rule 6 must keep enforcing its install-time hash.
///
/// `bundled` is the refreshed `~/.mur/mcp-servers/mur-mcp-server`, when the
/// supervisor refreshed one this start. `runtime_dir` is the directory holding
/// this runtime's own executable.
fn first_party_target(
    command: &str,
    bundled: Option<&Path>,
    runtime_dir: &Path,
) -> Option<PathBuf> {
    if let Some(bundled) = bundled {
        let c = Path::new(command);
        if c == bundled || c.file_name() == bundled.file_name() {
            return Some(bundled.to_path_buf());
        }
    }

    // Resolve exactly as the spawn does, so the file re-pinned is the file that
    // will run. Resolution canonicalizes, so a symlinked install (Homebrew's
    // `bin` into its Cellar) compares equal to the runtime's own resolved dir.
    let prog = command.split_whitespace().next()?;
    if !Path::new(prog)
        .file_name()?
        .to_str()?
        .starts_with(MUR_BINARY_PREFIX)
    {
        return None;
    }
    let resolved =
        mur_common::exec::resolve_command_in(&mur_common::exec::augmented_path_var(), prog).ok()?;
    (resolved.parent()? == runtime_dir).then_some(resolved)
}

/// Re-pin every profile entry that resolves to one of MUR's own binaries.
///
/// `bundled` is the refreshed `~/.mur/mcp-servers/mur-mcp-server` when the
/// supervisor refreshed one this start, `None` otherwise. `runtime_dir` is the
/// directory holding this runtime's own executable. Returns `true` when
/// `profile.yaml` was rewritten.
pub fn repin_first_party(
    agent_home: &Path,
    bundled: Option<&Path>,
    runtime_dir: &Path,
) -> anyhow::Result<bool> {
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)?;

    let mut changed = false;
    for entry in &mut profile.mcp_servers {
        let Some(target) = first_party_target(&entry.command, bundled, runtime_dir) else {
            continue;
        };
        let actual = match crate::hooks::b0_helpers::binary_sha256(&target) {
            Ok(a) => a,
            // Unreadable is the same call rule 6 makes for a missing binary:
            // far more likely "the user deleted it" than an attack, and a hard
            // error here would strand the agent with no way back in.
            Err(e) => {
                tracing::warn!(
                    mcp = %entry.name,
                    path = %target.display(),
                    error = %e,
                    "could not hash first-party MCP binary; leaving its pin alone",
                );
                continue;
            }
        };
        if entry.binary_sha256.as_deref() == Some(actual.as_str()) {
            continue;
        }
        tracing::info!(
            mcp = %entry.name,
            path = %target.display(),
            from = entry.binary_sha256.as_deref().unwrap_or("<unpinned>"),
            to = %actual,
            "re-pinned first-party MCP binary shipped with this runtime",
        );
        entry.binary_sha256 = Some(actual);
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

    /// For the bundled-arm tests: a runtime dir nothing resolves into, so only
    /// the `bundled` match can fire.
    fn no_runtime_dir() -> PathBuf {
        PathBuf::from("/nonexistent/runtime/dir")
    }

    /// Writes `name` into `dir` with `body` and returns its canonical path.
    fn sibling(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p.canonicalize().unwrap()
    }

    /// Writes a profile with one entry and returns (tempdir, canonical runtime dir).
    fn one_entry(command_from: impl FnOnce(&Path) -> String) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().join("bin");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        let runtime_dir = runtime_dir.canonicalize().unwrap();

        let yaml = include_str!("../tests/fixtures/profile_minimal.yaml");
        let mut profile: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        profile.mcp_servers = vec![entry(
            "gw",
            &command_from(&runtime_dir),
            Some(&"b".repeat(64)),
        )];
        std::fs::write(
            dir.path().join("profile.yaml"),
            serde_yaml_ng::to_string(&profile).unwrap(),
        )
        .unwrap();
        (dir, runtime_dir)
    }

    /// The regression this arm exists for: `mur-research-gateway` ships in the
    /// same release as the runtime, so every upgrade drifts its pin. Before
    /// this, rule 6 refused startup and every deep-research worker crash-looped.
    #[test]
    fn repins_a_first_party_sibling_shipped_with_this_runtime() {
        let (dir, runtime_dir) = one_entry(|rt| {
            sibling(rt, "mur-research-gateway", b"gateway v2")
                .display()
                .to_string()
        });

        assert!(repin_first_party(dir.path(), None, &runtime_dir).unwrap());

        let gw = runtime_dir.join("mur-research-gateway");
        let expected = crate::hooks::b0_helpers::binary_sha256(&gw).unwrap();
        assert_eq!(
            reload(dir.path()).mcp_servers[0].binary_sha256.as_deref(),
            Some(expected.as_str()),
        );
    }

    /// Directory alone is not the test. A binary wearing MUR's name from
    /// somewhere else on PATH is a stranger, and rule 6 must keep enforcing it.
    #[test]
    fn a_mur_named_binary_outside_the_runtime_dir_stays_enforced() {
        let elsewhere = tempfile::tempdir().unwrap();
        let (dir, runtime_dir) = one_entry(|_| {
            sibling(elsewhere.path(), "mur-research-gateway", b"impostor")
                .display()
                .to_string()
        });

        assert!(!repin_first_party(dir.path(), None, &runtime_dir).unwrap());
        assert_eq!(
            reload(dir.path()).mcp_servers[0].binary_sha256.as_deref(),
            Some("b".repeat(64).as_str()),
        );
    }

    /// Nor is the name alone. `~/.local/bin` is a crowded directory on a normal
    /// machine; a third-party MCP binary parked there must not lose its pin.
    #[test]
    fn a_third_party_binary_sharing_the_runtime_dir_stays_enforced() {
        let (dir, runtime_dir) =
            one_entry(|rt| sibling(rt, "weather-mcp", b"vendor").display().to_string());

        assert!(!repin_first_party(dir.path(), None, &runtime_dir).unwrap());
        assert_eq!(
            reload(dir.path()).mcp_servers[0].binary_sha256.as_deref(),
            Some("b".repeat(64).as_str()),
        );
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

        assert!(repin_first_party(dir.path(), Some(&bundled), &no_runtime_dir()).unwrap());

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

        assert!(repin_first_party(dir.path(), Some(&bundled), &no_runtime_dir()).unwrap());

        let expected = crate::hooks::b0_helpers::binary_sha256(&bundled).unwrap();
        assert_eq!(
            reload(dir.path()).mcp_servers[0].binary_sha256.as_deref(),
            Some(expected.as_str()),
        );
    }

    #[test]
    fn is_idempotent_and_does_not_rewrite_when_already_current() {
        let (dir, bundled) = fixture(|_| vec![entry("media", "mur-mcp-server", None)]);
        assert!(repin_first_party(dir.path(), Some(&bundled), &no_runtime_dir()).unwrap());

        let before = std::fs::read_to_string(dir.path().join("profile.yaml")).unwrap();
        assert!(
            !repin_first_party(dir.path(), Some(&bundled), &no_runtime_dir()).unwrap(),
            "second run has nothing to change"
        );
        assert_eq!(
            before,
            std::fs::read_to_string(dir.path().join("profile.yaml")).unwrap(),
            "an unchanged pin must not rewrite profile.yaml (updated_at would churn)",
        );
    }
}
