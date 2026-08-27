//! Local skill store helpers — list installed, resolve, remove, search, trust.

use crate::skill::store::{agent_skill_dir, global_skill_dir};
use crate::skill::types::TrustLevel;
use crate::skill::{SkillManifest, StoreError, read_from_dir};
use crate::trust::skills::SkillTrustStore;
use std::fs;
use std::path::{Path, PathBuf};

pub fn list_installed(mur_home: &Path) -> Result<Vec<String>, StoreError> {
    let skills_dir = mur_home.join("skills");
    if !skills_dir.exists() {
        return Ok(vec![]);
    }
    let mut names: Vec<_> = fs::read_dir(&skills_dir)
        .map_err(StoreError::Io)?
        .filter_map(|e| {
            let e = e.ok()?;
            if e.file_type().ok()?.is_dir() {
                let name = e.file_name().to_str()?.to_string();
                // ponytail: skip, don't migrate. Pre-fix fleet runs ledgered to
                // skills/fleet:<name>/ (see event_log_path); those directories
                // hold run history, never a manifest, and are not skills.
                (!name.starts_with("fleet:")).then_some(name)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn load_installed(mur_home: &Path, name: &str) -> Result<SkillManifest, StoreError> {
    read_from_dir(&global_skill_dir(mur_home, name))
}

pub fn list_installed_agent(mur_home: &Path, agent_name: &str) -> Result<Vec<String>, StoreError> {
    let dir = agent_skill_dir(mur_home, agent_name);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(StoreError::Io)?
        .filter_map(|e| {
            let e = e.ok()?;
            if e.file_type().ok()?.is_dir() {
                e.file_name().to_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn load_installed_agent(
    mur_home: &Path,
    agent_name: &str,
    skill: &str,
) -> Result<SkillManifest, StoreError> {
    read_from_dir(&agent_skill_dir(mur_home, agent_name).join(skill))
}

pub fn installed_path(mur_home: &Path, name: &str) -> PathBuf {
    global_skill_dir(mur_home, name)
}

pub fn remove_installed(mur_home: &Path, name: &str) -> Result<(), StoreError> {
    let dir = installed_path(mur_home, name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(StoreError::Io)?;
    }
    // Remove trust entry by name
    if let Ok(mut trust) = SkillTrustStore::load(mur_home) {
        trust.entries.retain(|_k, v| v.name != name);
        let _ = trust.save(mur_home);
    }
    Ok(())
}

pub fn search_installed(
    mur_home: &Path,
    query: &str,
) -> Result<Vec<(String, SkillManifest)>, StoreError> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    for name in list_installed(mur_home)? {
        if let Ok(m) = load_installed(mur_home, &name)
            && (name.to_lowercase().contains(&q)
                || m.description.to_lowercase().contains(&q)
                || m.tags.iter().any(|t| t.to_lowercase().contains(&q)))
        {
            results.push((name, m));
        }
    }
    Ok(results)
}

pub fn set_trust_level(
    mur_home: &Path,
    name: &str,
    level: TrustLevel,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut trust = SkillTrustStore::load(mur_home)?;
    let keys: Vec<String> = trust
        .entries
        .iter()
        .filter(|(_k, v)| v.name == name)
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        if let Some(e) = trust.entries.get_mut(&k) {
            e.level = level;
        }
    }
    trust.save(mur_home)?;
    Ok(())
}

pub fn get_trust_level(
    mur_home: &Path,
    name: &str,
) -> Result<TrustLevel, Box<dyn std::error::Error>> {
    let trust = SkillTrustStore::load(mur_home)?;
    for entry in trust.entries.values() {
        if entry.name == name {
            return Ok(entry.level);
        }
    }
    Ok(TrustLevel::Sandboxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{parse_canonical, write_to_dir};
    use tempfile::tempdir;

    fn sample(name: &str) -> SkillManifest {
        parse_canonical(&format!(
            r#"name: {name}
version: 1.0.0
publisher: human:t
description: test skill for {name}
category: context
content:
  abstract: hi
  context: body
tags: [test, {name}]
"#
        ))
        .unwrap()
    }

    #[test]
    fn list_returns_installed() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "a"), &sample("a")).unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "b"), &sample("b")).unwrap();
        assert_eq!(list_installed(dir.path()).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn empty_dir_returns_empty() {
        assert!(
            list_installed(tempdir().unwrap().path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_finds_by_name() {
        let dir = tempdir().unwrap();
        write_to_dir(
            &global_skill_dir(dir.path(), "my-prices"),
            &sample("my-prices"),
        )
        .unwrap();
        assert_eq!(search_installed(dir.path(), "price").unwrap().len(), 1);
    }

    #[test]
    fn search_finds_by_tag() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "web"), &sample("web")).unwrap();
        assert_eq!(search_installed(dir.path(), "test").unwrap().len(), 1);
    }

    #[test]
    fn remove_cleans_dir() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "rm-me"), &sample("rm-me")).unwrap();
        remove_installed(dir.path(), "rm-me").unwrap();
        assert!(list_installed(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn list_installed_ignores_legacy_fleet_ledgers() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "real"), &sample("real")).unwrap();
        // Pre-fix debris: a run ledger, no manifest.
        let legacy = dir.path().join("skills").join("fleet:builder");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("events.jsonl"), "{}\n").unwrap();
        assert_eq!(list_installed(dir.path()).unwrap(), vec!["real"]);
    }

    #[test]
    fn list_installed_agent_finds_agent_skills() {
        let dir = tempdir().unwrap();
        let agent_dir = agent_skill_dir(dir.path(), "alice");
        write_to_dir(&agent_dir.join("foo"), &sample("foo")).unwrap();
        let names = list_installed_agent(dir.path(), "alice").unwrap();
        assert_eq!(names, vec!["foo"]);
    }

    #[test]
    fn list_installed_agent_empty_when_dir_missing() {
        let dir = tempdir().unwrap();
        let names = list_installed_agent(dir.path(), "nobody").unwrap();
        assert!(names.is_empty());
    }
}
