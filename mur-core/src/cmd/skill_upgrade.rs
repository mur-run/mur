//! `mur skill upgrade` engine — scans origin-stamped skills (registry
//! installs; see `skill_install::stamp_registry_origin`) and brings them up
//! to the latest registry version, unless local content was modified.
//!
//! Fail-closed on every axis: one skill's failure never aborts the batch
//! (`UpgradeStatus::Error`), and a hash mismatch against the origin stamp
//! always blocks the write rather than silently clobbering a user edit.

use serde::Serialize;
use std::path::{Path, PathBuf};

use mur_common::skill::registry::RegistryIndex;
use mur_common::skill::{content_hash_for_origin, read_from_dir, write_to_dir};

use crate::cmd::skill_install::stamp_registry_origin;
use crate::cmd::skill_registry::skill_yaml_path;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpgradeStatus {
    UpToDate,
    Upgraded { from: String, to: String },
    BlockedModified { local: String, latest: String },
    NotInRegistry,
    Error(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct UpgradeItem {
    pub name: String,
    pub dir: PathBuf,
    pub status: UpgradeStatus,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UpgradeReport {
    pub items: Vec<UpgradeItem>,
}

/// Scan every origin-stamped skill under `~/.mur/skills/*` and
/// `~/.mur/agents/*/skills/*`, compare each against the registry cache at
/// `registry_dir`, and — when `apply` is true — upgrade what it safely can.
///
/// `apply=false` performs the same comparison but never writes; the report
/// reflects what an `apply=true` run would do (`Upgraded{from,to}` is used
/// for "would upgrade" in check mode too — see `mur skill upgrade --check`).
pub fn upgrade_all(mur_home: &Path, registry_dir: &Path, apply: bool) -> UpgradeReport {
    let mut items = Vec::new();

    let index = match load_index(registry_dir) {
        Ok(idx) => idx,
        Err(e) => {
            // Registry cache unreadable: fail-closed, report nothing rather
            // than guessing. Caller (CLI) surfaces this separately.
            tracing::warn!(error = %e, "skill upgrade: could not load registry index");
            RegistryIndex {
                schema_version: 0,
                updated_at: String::new(),
                skills: Default::default(),
            }
        }
    };

    for dir in discover_skill_dirs(mur_home) {
        if let Some(item) = evaluate_skill(&dir, &index, registry_dir, apply) {
            items.push(item);
        }
    }

    UpgradeReport { items }
}

fn load_index(registry_dir: &Path) -> anyhow::Result<RegistryIndex> {
    let path = registry_dir.join("index.yaml");
    let text = std::fs::read_to_string(&path)?;
    Ok(RegistryIndex::from_yaml(&text)?)
}

/// One skill dir per `~/.mur/skills/<name>` and `~/.mur/agents/<agent>/skills/<name>`.
fn discover_skill_dirs(mur_home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    push_skill_subdirs(&mur_home.join("skills"), &mut out);

    let agents_dir = mur_home.join("agents");
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                push_skill_subdirs(&entry.path().join("skills"), &mut out);
            }
        }
    }
    out
}

fn push_skill_subdirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && (path.join("skill.yaml").exists() || path.join("skill.md").exists()) {
            out.push(path);
        }
    }
}

fn evaluate_skill(
    dir: &Path,
    index: &RegistryIndex,
    registry_dir: &Path,
    apply: bool,
) -> Option<UpgradeItem> {
    let local = read_from_dir(dir).ok()?;
    let origin = local.origin.clone()?;
    let key = origin.strip_prefix("registry:")?;
    let (_, name) = key.rsplit_once('/').unwrap_or(("", key));
    let name = name.to_string();

    let Some(entry) = index.skills.get(&name) else {
        return Some(UpgradeItem {
            name,
            dir: dir.to_path_buf(),
            status: UpgradeStatus::NotInRegistry,
        });
    };

    let local_ver = local.origin_version.clone().unwrap_or_default();
    if entry.latest == local_ver {
        return Some(UpgradeItem {
            name,
            dir: dir.to_path_buf(),
            status: UpgradeStatus::UpToDate,
        });
    }

    let current_hash = content_hash_for_origin(&local).ok();
    let modified = local.origin_hash != current_hash;
    if modified {
        return Some(UpgradeItem {
            name,
            dir: dir.to_path_buf(),
            status: UpgradeStatus::BlockedModified {
                local: local_ver,
                latest: entry.latest.clone(),
            },
        });
    }

    if !apply {
        return Some(UpgradeItem {
            name,
            dir: dir.to_path_buf(),
            status: UpgradeStatus::Upgraded {
                from: local_ver,
                to: entry.latest.clone(),
            },
        });
    }

    let status = match apply_upgrade(dir, registry_dir, &name, &entry.latest) {
        Ok(()) => UpgradeStatus::Upgraded {
            from: local_ver,
            to: entry.latest.clone(),
        },
        Err(e) => UpgradeStatus::Error(e.to_string()),
    };
    Some(UpgradeItem {
        name,
        dir: dir.to_path_buf(),
        status,
    })
}

fn apply_upgrade(dir: &Path, registry_dir: &Path, name: &str, latest: &str) -> anyhow::Result<()> {
    let yaml_path = skill_yaml_path(registry_dir, name, latest);
    let text = std::fs::read_to_string(&yaml_path)?;
    let mut manifest = mur_common::skill::parse_canonical(&text)?;
    mur_common::skill::validate(&manifest)?;
    stamp_registry_origin(&mut manifest);
    write_to_dir(dir, &manifest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_registry_entry(
        registry_dir: &Path,
        name: &str,
        version: &str,
        yaml: &str,
    ) {
        let path = skill_yaml_path(registry_dir, name, version);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, yaml).unwrap();
    }

    fn write_index(registry_dir: &Path, name: &str, latest: &str) {
        let index_yaml = format!(
            "schema_version: 1\nupdated_at: \"2026-01-01\"\nskills:\n  {name}:\n    latest: \"{latest}\"\n    description: d\n    publisher: human:mur-official\n    category: context\n"
        );
        std::fs::write(registry_dir.join("index.yaml"), index_yaml).unwrap();
    }

    fn install_local_skill(
        mur_home: &Path,
        name: &str,
        version: &str,
        stamp_hash_from: Option<&mur_common::skill::SkillManifest>,
    ) -> PathBuf {
        let yaml = format!(
            "name: {name}\nversion: {version}\npublisher: human:mur-official\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: body\n"
        );
        let mut m = mur_common::skill::parse_canonical(&yaml).unwrap();
        m.origin = Some(format!("registry:human:mur-official/{name}"));
        m.origin_version = Some(version.to_string());
        m.origin_hash = Some(
            content_hash_for_origin(stamp_hash_from.unwrap_or(&m))
                .unwrap(),
        );
        let dir = mur_home.join("skills").join(name);
        write_to_dir(&dir, &m).unwrap();
        dir
    }

    #[test]
    fn unmodified_skill_upgrades_and_restamps() {
        let home = TempDir::new().unwrap();
        let registry = TempDir::new().unwrap();

        let v1_yaml = "name: t\nversion: 1.0.0\npublisher: human:mur-official\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";
        let v2_yaml = "name: t\nversion: 2.0.0\npublisher: human:mur-official\ndescription: d2\ncategory: context\ncontent:\n  abstract: a2\n  context: body2\n";
        write_registry_entry(registry.path(), "t", "1.0.0", v1_yaml);
        write_registry_entry(registry.path(), "t", "2.0.0", v2_yaml);
        write_index(registry.path(), "t", "2.0.0");

        let dir = install_local_skill(home.path(), "t", "1.0.0", None);

        let report = upgrade_all(home.path(), registry.path(), true);
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].status,
            UpgradeStatus::Upgraded {
                from: "1.0.0".into(),
                to: "2.0.0".into()
            }
        );

        let upgraded = read_from_dir(&dir).unwrap();
        assert_eq!(upgraded.version, "2.0.0");
        assert_eq!(upgraded.origin_version.as_deref(), Some("2.0.0"));
        assert_eq!(
            upgraded.origin_hash.as_deref().unwrap(),
            content_hash_for_origin(&upgraded).unwrap()
        );
    }

    #[test]
    fn modified_skill_is_never_overwritten() {
        let home = TempDir::new().unwrap();
        let registry = TempDir::new().unwrap();

        let v1_yaml = "name: t\nversion: 1.0.0\npublisher: human:mur-official\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";
        let v2_yaml = "name: t\nversion: 2.0.0\npublisher: human:mur-official\ndescription: d2\ncategory: context\ncontent:\n  abstract: a2\n  context: body2\n";
        write_registry_entry(registry.path(), "t", "1.0.0", v1_yaml);
        write_registry_entry(registry.path(), "t", "2.0.0", v2_yaml);
        write_index(registry.path(), "t", "2.0.0");

        let dir = install_local_skill(home.path(), "t", "1.0.0", None);
        // User edits the installed skill locally after install.
        let mut local = read_from_dir(&dir).unwrap();
        local.description = "user tweaked this".into();
        write_to_dir(&dir, &local).unwrap();
        let before_bytes = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();

        let report = upgrade_all(home.path(), registry.path(), true);
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].status,
            UpgradeStatus::BlockedModified {
                local: "1.0.0".into(),
                latest: "2.0.0".into()
            }
        );

        // On-disk content is byte-identical: never overwritten.
        let after_bytes = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(before_bytes, after_bytes);
    }

    #[test]
    fn check_mode_writes_nothing() {
        let home = TempDir::new().unwrap();
        let registry = TempDir::new().unwrap();

        let v1_yaml = "name: t\nversion: 1.0.0\npublisher: human:mur-official\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";
        let v2_yaml = "name: t\nversion: 2.0.0\npublisher: human:mur-official\ndescription: d2\ncategory: context\ncontent:\n  abstract: a2\n  context: body2\n";
        write_registry_entry(registry.path(), "t", "1.0.0", v1_yaml);
        write_registry_entry(registry.path(), "t", "2.0.0", v2_yaml);
        write_index(registry.path(), "t", "2.0.0");

        let dir = install_local_skill(home.path(), "t", "1.0.0", None);
        let before_bytes = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();

        let report = upgrade_all(home.path(), registry.path(), false);
        assert_eq!(report.items.len(), 1);
        assert_eq!(
            report.items[0].status,
            UpgradeStatus::Upgraded {
                from: "1.0.0".into(),
                to: "2.0.0".into()
            }
        );

        let after_bytes = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(before_bytes, after_bytes, "check mode must never write");
    }

    #[test]
    fn unstamped_and_unknown_skills_are_skipped() {
        let home = TempDir::new().unwrap();
        let registry = TempDir::new().unwrap();
        write_index(registry.path(), "known", "1.0.0");

        // Unstamped local-author skill: never touched by upgrade.
        let unstamped_yaml = "name: u\nversion: 1.0.0\npublisher: human:me\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";
        let m = mur_common::skill::parse_canonical(unstamped_yaml).unwrap();
        write_to_dir(&home.path().join("skills").join("u"), &m).unwrap();

        // Stamped but absent from the registry index: NotInRegistry.
        let dir = install_local_skill(home.path(), "gone", "1.0.0", None);

        let report = upgrade_all(home.path(), registry.path(), true);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].name, "gone");
        assert_eq!(report.items[0].status, UpgradeStatus::NotInRegistry);

        // Unstamped skill's content is untouched.
        let unstamped_after =
            std::fs::read_to_string(home.path().join("skills").join("u").join("skill.yaml"))
                .unwrap();
        assert!(unstamped_after.contains("name: u"));
        let _ = dir; // silence unused warning if layout changes
    }
}
