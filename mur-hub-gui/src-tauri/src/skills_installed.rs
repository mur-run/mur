//! Installed-skills list for the Hub's Skills library page.
//!
//! Reads every globally-installed skill under `~/.mur/skills/*/skill.yaml`
//! and annotates each with the same upgrade-status classification Phase 2's
//! Home-inbox status uses (`mur_core::cmd::skill_upgrade::upgrade_all`),
//! run once in check mode against the cached registry index.
//!
//! Fail-open on every axis: a single skill that fails to parse is skipped
//! (+ `tracing::warn`); any top-level error (missing skills dir, IO error)
//! yields an empty list rather than a command error the UI must handle.

use std::collections::HashMap;
use std::path::Path;

use mur_common::skill::store::read_from_dir;
use mur_core::cmd::skill_registry::registry_cache_dir;
use mur_core::cmd::skill_upgrade::{UpgradeStatus, upgrade_all};
use serde::Serialize;

/// One installed skill as shown in the Skills library list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub description: String,
    pub category: String,
    pub origin_version: Option<String>,
    pub status: String,
    /// Agents whose profile lists this skill, sorted by name.
    pub agents: Vec<String>,
    /// The global skill directory (for "Open folder"); `None` only if the
    /// path is not valid UTF-8.
    pub path: Option<String>,
}

/// Pure mapping: `UpgradeStatus` -> the short display label shown in the
/// status column. Kept separate from I/O so it's unit-testable without a
/// real registry cache or filesystem.
pub fn status_label(status: &UpgradeStatus) -> String {
    match status {
        UpgradeStatus::UpToDate => "up to date".to_string(),
        UpgradeStatus::Upgraded { .. } => "update available".to_string(),
        UpgradeStatus::BlockedModified { .. } => "modified".to_string(),
        UpgradeStatus::NotInRegistry => "—".to_string(),
        UpgradeStatus::Error(_) => "—".to_string(),
    }
}

/// Pure fold: (agent name, its `profile.installed_skills`) pairs → skill
/// name → sorted agent names. Kept free of I/O so it is unit-testable.
pub fn agents_by_skill(
    profiles: &[(String, Vec<mur_common::agent::SkillCardEntry>)],
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (agent, cards) in profiles {
        for card in cards {
            map.entry(card.name.clone())
                .or_default()
                .push(agent.clone());
        }
    }
    for agents in map.values_mut() {
        agents.sort();
        agents.dedup();
    }
    map
}

/// Read every `agents/*/profile.yaml` the way `mcp_installed` does; an
/// unreadable or unparsable profile is skipped with a warning.
fn read_agent_skill_cards(
    agents_dir: &Path,
) -> Vec<(String, Vec<mur_common::agent::SkillCardEntry>)> {
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %agents_dir.display(), error = %e, "skills_installed: cannot read agents dir");
            return vec![];
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        match std::fs::read(dir.join("profile.yaml")) {
            Ok(bytes) => match serde_yaml_ng::from_slice::<mur_common::AgentProfile>(&bytes) {
                Ok(profile) => out.push((agent_name.to_string(), profile.installed_skills.clone())),
                Err(e) => {
                    tracing::warn!(agent = %agent_name, error = %e, "skills_installed: skipping unparsable profile")
                }
            },
            Err(e) => {
                tracing::warn!(agent = %agent_name, error = %e, "skills_installed: skipping unreadable profile")
            }
        }
    }
    out
}

/// List every skill under `~/.mur/skills/*` with its upgrade status.
/// Fail-open: any error surfaces as an empty list plus a `tracing::warn`.
#[tauri::command]
pub fn skills_installed() -> Result<Vec<InstalledSkillView>, String> {
    let mur_home = crate::mur_home_path();
    let skills_dir = mur_home.join("skills");

    if !skills_dir.exists() {
        return Ok(vec![]);
    }

    let registry_dir = registry_cache_dir(&mur_home);
    let status_by_name: HashMap<String, UpgradeStatus> = if registry_dir.exists() {
        upgrade_all(&mur_home, &registry_dir, false)
            .items
            .into_iter()
            .map(|item| (item.name, item.status))
            .collect()
    } else {
        tracing::warn!(
            dir = %registry_dir.display(),
            "skills_installed: registry cache absent, statuses will show \"—\""
        );
        HashMap::new()
    };

    let usage = agents_by_skill(&read_agent_skill_cards(&mur_home.join("agents")));
    Ok(list_skills(&skills_dir, &status_by_name, &usage))
}

fn list_skills(
    skills_dir: &Path,
    status_by_name: &HashMap<String, UpgradeStatus>,
    usage: &std::collections::BTreeMap<String, Vec<String>>,
) -> Vec<InstalledSkillView> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %skills_dir.display(), error = %e, "skills_installed: cannot read skills dir");
            return vec![];
        }
    };

    let mut views = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        match read_from_dir(&dir) {
            Ok(manifest) => {
                let status = status_by_name
                    .get(&manifest.name)
                    .map(status_label)
                    .unwrap_or_else(|| "—".to_string());
                views.push(InstalledSkillView {
                    name: manifest.name.clone(),
                    description: manifest.description.clone(),
                    category: format!("{:?}", manifest.category).to_lowercase(),
                    origin_version: manifest.origin.as_ref().map(|_| manifest.version.clone()),
                    status,
                    agents: usage.get(&manifest.name).cloned().unwrap_or_default(),
                    path: dir.to_str().map(str::to_string),
                });
            }
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "skills_installed: skipping unreadable skill");
            }
        }
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_label_maps_all_variants() {
        assert_eq!(status_label(&UpgradeStatus::UpToDate), "up to date");
        assert_eq!(
            status_label(&UpgradeStatus::Upgraded {
                from: "1.0.0".into(),
                to: "2.0.0".into(),
            }),
            "update available"
        );
        assert_eq!(
            status_label(&UpgradeStatus::BlockedModified {
                local: "1.0.0".into(),
                latest: "2.0.0".into(),
            }),
            "modified"
        );
        assert_eq!(status_label(&UpgradeStatus::NotInRegistry), "—");
        assert_eq!(status_label(&UpgradeStatus::Error("boom".into())), "—");
    }

    #[test]
    fn agents_by_skill_folds_agents_per_skill_sorted() {
        use mur_common::agent::SkillCardEntry;
        let card = |name: &str| SkillCardEntry {
            name: name.to_string(),
            ..Default::default()
        };
        let profiles = vec![
            ("scout".to_string(), vec![card("mur-dev"), card("mur-tdd")]),
            ("aura".to_string(), vec![card("mur-dev")]),
            ("muse".to_string(), vec![]),
        ];
        let map = agents_by_skill(&profiles);
        assert_eq!(
            map.get("mur-dev").unwrap(),
            &vec!["aura".to_string(), "scout".to_string()]
        );
        assert_eq!(map.get("mur-tdd").unwrap(), &vec!["scout".to_string()]);
        assert!(map.get("nope").is_none());
    }

    #[test]
    fn list_skills_empty_dir_returns_empty() {
        let tmp =
            std::env::temp_dir().join(format!("mur-skills-installed-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let result = list_skills(&tmp, &HashMap::new(), &std::collections::BTreeMap::new());
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_skills_reads_manifest_and_maps_status() {
        let tmp =
            std::env::temp_dir().join(format!("mur-skills-installed-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skill_dir = tmp.join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            r#"
name: my-skill
version: "1.0.0"
publisher: acme
description: A test skill
category: workflow
content:
  abstract: "does a thing"
  context: "when you need a thing done"
triggers: []
"#,
        )
        .unwrap();

        let mut statuses = HashMap::new();
        statuses.insert(
            "my-skill".to_string(),
            UpgradeStatus::Upgraded {
                from: "1.0.0".into(),
                to: "1.1.0".into(),
            },
        );

        let mut usage = std::collections::BTreeMap::new();
        usage.insert("my-skill".to_string(), vec!["aura".to_string()]);

        let result = list_skills(&tmp, &statuses, &usage);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my-skill");
        assert_eq!(result[0].category, "workflow");
        assert_eq!(result[0].status, "update available");
        assert_eq!(result[0].agents, vec!["aura".to_string()]);
        assert!(result[0].path.is_some());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
