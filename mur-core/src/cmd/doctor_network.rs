//! `mur doctor` → `check_network_entitlements` (#1906, spec Part 4).
//!
//! Runs the shared write-time validator over every agent's stored host lists.
//! `--fix` removes what it refuses, after a per-agent preview in the same
//! `+ / -` shape every other egress writer shows (spec Part 2). A
//! `Restricted` server left with no hosts is flagged, never re-moded.

use std::path::Path;

use anyhow::Result;
use mur_common::AgentProfile;
use mur_common::host_pattern::audit::{HostList, HostRepair, invalid_hosts, remove_invalid_hosts};

/// One agent with at least one invalid entry.
pub(crate) struct AgentFinding {
    pub agent: String,
    pub profile: AgentProfile,
}

/// Every agent under `mur_home` whose profile holds an invalid host entry,
/// in name order. Unreadable profiles are skipped: other checks report them.
pub(crate) fn scan(mur_home: &Path) -> Vec<AgentFinding> {
    let Ok(entries) = std::fs::read_dir(mur_home.join("agents")) else {
        return Vec::new();
    };
    let mut dirs: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| {
            let yaml = std::fs::read_to_string(dir.join("profile.yaml")).ok()?;
            let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).ok()?;
            let agent = dir.file_name()?.to_string_lossy().into_owned();
            (!invalid_hosts(&profile).is_empty()).then_some(AgentFinding { agent, profile })
        })
        .collect()
}

/// Doctor's report lines for `findings`.
pub(crate) fn report(findings: &[AgentFinding]) -> Vec<String> {
    if findings.is_empty() {
        return vec!["✅ Network entitlements: every stored host entry is a valid host".into()];
    }
    let total: usize = findings
        .iter()
        .map(|f| invalid_hosts(&f.profile).len())
        .sum();
    let mut out = vec![format!(
        "❌ Network entitlements: {total} invalid host entr(ies) match nothing"
    )];
    for f in findings {
        for b in invalid_hosts(&f.profile) {
            out.push(format!(
                "  ⚠ agent `{}` · {} · `{}` ({})",
                f.agent, b.list, b.value, b.reason
            ));
        }
    }
    out
}

/// The `--fix` preview for one agent: removed lines grouped per list, then
/// any `Restricted` server the fix leaves empty.
pub(crate) fn fix_preview(agent: &str, repair: &HostRepair) -> String {
    let mut out = vec![format!("agent `{agent}` — remove invalid host entries:")];
    let mut last: Option<&HostList> = None;
    for b in &repair.removed {
        if last != Some(&b.list) {
            out.push(format!("  {}:", b.list));
            last = Some(&b.list);
        }
        out.push(format!("    - {}", b.value));
    }
    for s in &repair.emptied_restricted {
        out.push(format!(
            "  ⚠ `{s}` will have no hosts left and reach nothing. Its mode is not changed; \
             name its sites with `/browser live <host>` or `mur agent mcp set-network`."
        ));
    }
    out.push("Saved to the profile; takes effect after the agent restarts.".into());
    out.join("\n")
}

/// Interactive `--fix`: preview, confirm, write — one agent at a time, so a
/// "no" for one never blocks another. `confirm` is injected for tests.
pub(crate) fn run_fix(
    mur_home: &Path,
    findings: Vec<AgentFinding>,
    confirm: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<()> {
    for f in findings {
        let mut profile = f.profile;
        let repair = remove_invalid_hosts(&mut profile);
        if repair.removed.is_empty() {
            continue;
        }
        println!("\n{}", fix_preview(&f.agent, &repair));
        if !confirm("   Apply this fix?")? {
            println!("   Skipped.");
            continue;
        }
        let path = mur_home.join("agents").join(&f.agent).join("profile.yaml");
        match crate::cmd::agent::save_profile(&path, &mut profile) {
            Ok(()) => println!("   ✅ removed {} entr(ies)", repair.removed.len()),
            // Reported, not fatal: the next agent still gets its turn.
            Err(e) => println!("   ❌ Fix failed: {e:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = include_str!("agent/network_view_fixture.yaml");

    fn write_agent(home: &Path, name: &str, allow: &[&str]) {
        let mut p: AgentProfile = serde_yaml_ng::from_str(YAML).unwrap();
        p.name = name.into();
        p.entitlements.network.outbound.allow_hosts = allow.iter().map(|s| (*s).into()).collect();
        let mut e: mur_common::agent::McpServerEntry =
            serde_yaml_ng::from_str("name: browser\ncommand: x").unwrap();
        e.network = Some(mur_common::agent::McpServerNetwork {
            mode: mur_common::agent::McpNetMode::Restricted,
            allow_hosts: vec!["查台灣居家修繕市場規模".into(), "／".into(), "競品".into()],
            ..Default::default()
        });
        p.mcp_servers = vec![e];
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn reports_the_1905_values_and_preview_flags_the_emptied_server() {
        let home = tempfile::tempdir().unwrap();
        write_agent(home.path(), "mur", &["api.example.com", "評分，"]);
        let found = scan(home.path());
        assert_eq!(found.len(), 1);
        let r = report(&found).join("\n");
        assert!(r.contains("4 invalid"), "{r}");
        assert!(
            r.contains("agent `mur` · MCP server `browser` allow_hosts · `競品`"),
            "{r}"
        );
        let mut p = found[0].profile.clone();
        let preview = fix_preview("mur", &remove_invalid_hosts(&mut p));
        assert!(
            preview.contains("    - 查台灣居家修繕市場規模"),
            "{preview}"
        );
        assert!(
            preview.contains("`browser` will have no hosts left"),
            "{preview}"
        );
        assert!(!preview.contains("api.example.com"), "{preview}");
    }

    #[test]
    fn clean_home_reports_ok() {
        let home = tempfile::tempdir().unwrap();
        assert!(report(&scan(home.path()))[0].starts_with("✅"));
    }
}
