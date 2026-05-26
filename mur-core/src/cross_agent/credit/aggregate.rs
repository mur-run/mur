//! Credit aggregation across peer agents (M7c §6.2).
//!
//! Reads each peer's ledger, collects entries for a given skill, and
//! synthesises mutator/recombiner entries from the manifest's
//! `evolution_log` when the ledger is empty (graceful pre-M7c history).

use std::path::Path;

use anyhow::Result;
use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
use mur_common::skill::peers::list_peer_agents;

use crate::cross_agent::credit::ledger::read_for_skill;

#[derive(Debug)]
pub struct CreditView {
    pub skill: String,
    pub entries: Vec<CreditEntry>,
}

pub fn build_credit_view(home: &Path, invoking_agent: &str, skill: &str) -> Result<CreditView> {
    let mut entries: Vec<CreditEntry> = Vec::new();
    // Self ledger first.
    entries.extend(read_for_skill(home, invoking_agent, skill)?);
    // Peer ledgers.
    for peer in list_peer_agents(home)? {
        if peer.name == invoking_agent {
            continue;
        }
        entries.extend(read_for_skill(home, &peer.name, skill)?);
    }
    // Evolution-log fallback for mutator entries that predate the ledger.
    let mut synth_seen: std::collections::HashSet<(String, String, String)> = entries
        .iter()
        .filter(|e| matches!(e.kind, CreditKind::Mutator))
        .map(|e| (e.agent.clone(), e.skill.clone(), e.skill_version.clone()))
        .collect();

    let candidates: Vec<String> = vec![invoking_agent.to_string()]
        .into_iter()
        .chain(
            list_peer_agents(home)?
                .into_iter()
                .filter(|p| p.name != invoking_agent)
                .map(|p| p.name),
        )
        .collect();

    for agent in &candidates {
        let manifest_path = home
            .join("agents")
            .join(agent)
            .join("skills")
            .join(skill)
            .join("skill.yaml");
        if !manifest_path.exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        let Ok(m) =
            serde_yaml_ng::from_slice::<mur_common::skill::SkillManifest>(&bytes)
        else {
            continue;
        };
        for evt in &m.evolution_log {
            if evt.generation == 0 {
                continue; // initial-human → already covered by Author ledger entry
            }
            let key = (agent.clone(), skill.to_string(), evt.version.clone());
            if synth_seen.contains(&key) {
                continue;
            }
            let from_version =
                previous_version(&m.evolution_log, &evt.version).unwrap_or_else(|| "?".to_string());
            entries.push(CreditEntry {
                ts: evt
                    .timestamp
                    .parse()
                    .unwrap_or_else(|_| chrono::Utc::now()),
                skill: skill.to_string(),
                skill_version: evt.version.clone(),
                kind: CreditKind::Mutator,
                agent: agent.clone(),
                evidence: Some(CreditEvidence::Mutator {
                    from_version,
                    diff_summary: evt.changes.clone(),
                }),
                source: evt.source.clone(),
            });
            synth_seen.insert(key);
        }
    }

    entries.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(CreditView {
        skill: skill.to_string(),
        entries,
    })
}

fn previous_version(
    log: &[mur_common::skill::EvolutionEvent],
    target: &str,
) -> Option<String> {
    let mut prior = None;
    for evt in log {
        if evt.version == target {
            return prior;
        }
        prior = Some(evt.version.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn returns_empty_view_when_no_data() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let v = build_credit_view(home, "alice", "nonexistent").unwrap();
        assert!(v.entries.is_empty());
    }
}
