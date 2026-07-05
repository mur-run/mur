//! Blocked-upgrade status for the Home inbox.
//!
//! Thin wrapper around `mur_core::cmd::skill_upgrade::upgrade_all` (in-process
//! call, no CLI subprocess) run in check mode (`apply = false`) against the
//! *cached* registry index — this never triggers a network fetch, so it is
//! safe to call from a hot Home-screen status path.
//!
//! Fail-open on every axis: a missing registry cache or any error surfaces
//! as an empty list plus a `tracing::warn`, never an error the UI must
//! handle specially.

use std::path::Path;

use mur_core::cmd::skill_registry::registry_cache_dir;
use mur_core::cmd::skill_upgrade::{UpgradeReport, UpgradeStatus, upgrade_all};
use serde::Serialize;

/// One skill whose local copy was modified and can't be safely
/// auto-upgraded; surfaced so the user can review/resolve it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockedItem {
    pub name: String,
    pub dir: String,
    pub local_version: String,
    pub latest_version: String,
}

/// Pure mapping: extract only `BlockedModified` items from an upgrade
/// report. Kept separate from I/O so it's unit-testable without a real
/// registry cache or filesystem.
pub fn blocked_items(report: &UpgradeReport) -> Vec<BlockedItem> {
    report
        .items
        .iter()
        .filter_map(|item| match &item.status {
            UpgradeStatus::BlockedModified { local, latest } => Some(BlockedItem {
                name: item.name.clone(),
                dir: item.dir.display().to_string(),
                local_version: local.clone(),
                latest_version: latest.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Run the check in-process against the cached registry index and return
/// only the blocked (locally-modified) items. Fail-open: registry cache
/// absent or any error yields an empty list rather than an error.
#[tauri::command]
pub fn skill_upgrade_status() -> Result<Vec<BlockedItem>, String> {
    let mur_home = crate::mur_home_path();
    let registry_dir = registry_cache_dir(&mur_home);

    if !registry_dir.exists() {
        tracing::warn!(
            dir = %registry_dir.display(),
            "skill_upgrade_status: registry cache absent, skipping (fail-open)"
        );
        return Ok(vec![]);
    }

    let report = check_report(&mur_home, &registry_dir);
    Ok(blocked_items(&report))
}

fn check_report(mur_home: &Path, registry_dir: &Path) -> UpgradeReport {
    upgrade_all(mur_home, registry_dir, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_core::cmd::skill_upgrade::UpgradeItem;
    use std::path::PathBuf;

    fn item(name: &str, status: UpgradeStatus) -> UpgradeItem {
        UpgradeItem {
            name: name.to_string(),
            dir: PathBuf::from(format!("/tmp/{name}")),
            status,
        }
    }

    #[test]
    fn blocked_items_maps_only_blocked_modified() {
        let report = UpgradeReport {
            items: vec![
                item(
                    "upgraded-one",
                    UpgradeStatus::Upgraded {
                        from: "1.0.0".into(),
                        to: "2.0.0".into(),
                    },
                ),
                item(
                    "blocked-one",
                    UpgradeStatus::BlockedModified {
                        local: "1.0.0".into(),
                        latest: "3.0.0".into(),
                    },
                ),
                item("uptodate-one", UpgradeStatus::UpToDate),
            ],
        };

        let blocked = blocked_items(&report);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].name, "blocked-one");
        assert_eq!(blocked[0].dir, "/tmp/blocked-one");
        assert_eq!(blocked[0].local_version, "1.0.0");
        assert_eq!(blocked[0].latest_version, "3.0.0");
    }

    #[test]
    fn blocked_items_empty_when_none_blocked() {
        let report = UpgradeReport {
            items: vec![
                item("a", UpgradeStatus::UpToDate),
                item("b", UpgradeStatus::NotInRegistry),
                item("c", UpgradeStatus::Error("boom".into())),
            ],
        };
        assert!(blocked_items(&report).is_empty());
    }
}
