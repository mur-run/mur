//! `mur agent history/rollback` — versioned agent commands (E1 W3).

use crate::store::versioned::agent::VersionedAgentStore;
use anyhow::{Context, Result};
use std::path::Path;

use crate::store::yaml::default_mur_dir;

fn agents_dir() -> std::path::PathBuf {
    default_mur_dir().join("agents")
}

fn open_or_init(root: &Path) -> Result<VersionedAgentStore> {
    if root.join(".git").exists() {
        VersionedAgentStore::open(root)
            .with_context(|| format!("open versioned agent store at {}", root.display()))
    } else {
        VersionedAgentStore::init(root)
            .with_context(|| format!("init versioned agent store at {}", root.display()))
    }
}

// ── history ───────────────────────────────────────────────────────────────────

pub(crate) fn cmd_agent_history(name: &str) -> Result<()> {
    let root = agents_dir();
    let store = open_or_init(&root)?;

    let hist = store.history(name)?;
    if hist.is_empty() {
        println!("No version history for agent '{name}'.");
        println!("(History is recorded starting from the first save after versioned store init.)");
        return Ok(());
    }

    println!("History for agent '{name}':");
    println!("{:<5} {:<14} {:<26} reason", "v", "sha", "timestamp");
    println!("{}", "-".repeat(70));
    for e in &hist {
        let ts = chrono::DateTime::from_timestamp(e.timestamp, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("{:<5} {:<14} {:<26} {}", e.version, e.sha, ts, e.reason);
    }
    Ok(())
}

// ── rollback ──────────────────────────────────────────────────────────────────

pub(crate) fn cmd_agent_rollback(name: &str, to: u32) -> Result<()> {
    let root = agents_dir();
    let mut store = open_or_init(&root)?;

    let current_v = store.current_version(name);
    if current_v == 0 {
        anyhow::bail!("Agent '{name}' has no version history — cannot rollback.");
    }
    if to >= current_v {
        anyhow::bail!(
            "Cannot rollback agent '{name}' to v{to}: current version is v{current_v}. \
             Choose a version less than v{current_v}."
        );
    }

    let out = store.rollback_profile(name, to)?;
    for line in rollback_summary(name, to, &out) {
        println!("{line}");
    }
    Ok(())
}

/// What `mur agent rollback` tells the user: the new revision, any changed
/// entitlement keys (a rollback can widen them), and a reseal hint when the
/// pin was left behind so the agent would refuse to start.
fn rollback_summary(
    name: &str,
    to: u32,
    out: &crate::store::versioned::agent::RollbackOutcome,
) -> Vec<String> {
    let rev = &out.revision;
    let mut lines = vec![format!(
        "Rolled back agent '{name}' profile to v{to} → new commit v{} ({})",
        rev.version, rev.sha
    )];
    if !out.changed_entitlements.is_empty() {
        lines.push(format!(
            "Entitlements changed: {}. Review with `mur agent perm {name}`.",
            out.changed_entitlements.join(", ")
        ));
    }
    if !out.pin_advanced {
        lines.push(format!(
            "Entitlements pin not advanced: the profile it replaced did not match its pin. \
             Review, then run `mur agent perm reseal {name}` before starting the agent."
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::rollback_summary;
    use crate::store::versioned::agent::{AgentRevision, RollbackOutcome};

    fn outcome(changed: &[&str], pin_advanced: bool) -> RollbackOutcome {
        RollbackOutcome {
            revision: AgentRevision {
                name: "a".into(),
                version: 3,
                sha: "abc123def456".into(),
            },
            changed_entitlements: changed.iter().map(|s| s.to_string()).collect(),
            pin_advanced,
        }
    }

    #[test]
    fn summary_lists_changed_entitlements() {
        let lines = rollback_summary("a", 1, &outcome(&["filesystem", "network"], true));
        assert!(
            lines[0].contains("v1") && lines[0].contains("v3"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("filesystem, network")),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("reseal")), "{lines:?}");
    }

    #[test]
    fn summary_warns_to_reseal_when_pin_did_not_advance() {
        let lines = rollback_summary("a", 1, &outcome(&[], false));
        assert!(
            lines.iter().any(|l| l.contains("mur agent perm reseal a")),
            "{lines:?}"
        );
    }

    #[test]
    fn summary_is_one_line_when_nothing_changed() {
        let lines = rollback_summary("a", 1, &outcome(&[], true));
        assert_eq!(lines.len(), 1, "{lines:?}");
    }
}
