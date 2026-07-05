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

    Ok(list_skills(&skills_dir, &status_by_name))
}

fn list_skills(
    skills_dir: &Path,
    status_by_name: &HashMap<String, UpgradeStatus>,
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
    fn list_skills_empty_dir_returns_empty() {
        let tmp =
            std::env::temp_dir().join(format!("mur-skills-installed-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let result = list_skills(&tmp, &HashMap::new());
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

        let result = list_skills(&tmp, &statuses);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my-skill");
        assert_eq!(result[0].category, "workflow");
        assert_eq!(result[0].status, "update available");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
