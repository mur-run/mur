//! Skill registry client — shallow clone via git, load index, search.
//!
//! Default registry: https://github.com/mur-run/skill-registry.git
//! Cached at: ~/.mur/cache/registry/ (shallow clone, refreshed)

use anyhow::{Context, Result};
use mur_common::skill::registry::{RegistryIndex, RegistrySkillEntry};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_REGISTRY: &str = "https://github.com/mur-run/skill-registry.git";

pub fn registry_cache_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join("registry")
}

pub fn fetch_registry(mur_home: &Path, registry_url: &str) -> Result<PathBuf> {
    let cache_dir = registry_cache_dir(mur_home);
    let git_dir = cache_dir.join(".git");

    if git_dir.exists() {
        let status = Command::new("git")
            .args(["-C", &*cache_dir.to_string_lossy(), "pull", "--depth=1", "--ff-only"])
            .status()
            .map_err(|e| anyhow::anyhow!("git pull: {e}"))?;
        if !status.success() {
            eprintln!("warning: registry refresh failed, using cached");
        }
    } else {
        let parent = cache_dir.parent().unwrap_or(mur_home);
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
        let status = Command::new("git")
            .args(["clone", "--depth=1", registry_url, &*cache_dir.to_string_lossy()])
            .status()
            .map_err(|e| anyhow::anyhow!("git clone: {e}"))?;
        if !status.success() {
            anyhow::bail!("failed to clone registry from {registry_url}");
        }
    }
    Ok(cache_dir)
}

pub fn load_index(registry_dir: &Path) -> Result<RegistryIndex> {
    let p = registry_dir.join("index.yaml");
    let text = std::fs::read_to_string(&p)
        .with_context(|| format!("read {}", p.display()))?;
    RegistryIndex::from_yaml(&text)
        .map_err(|e| anyhow::anyhow!("parse index: {e}"))
}

pub fn fetch_and_load(mur_home: &Path, url: &str) -> Result<(PathBuf, RegistryIndex)> {
    let dir = fetch_registry(mur_home, url)?;
    let idx = load_index(&dir)?;
    Ok((dir, idx))
}

pub fn skill_yaml_path(registry_dir: &Path, name: &str, version: &str) -> PathBuf {
    registry_dir.join("skills").join(name).join("versions").join(format!("{version}.yaml"))
}

pub fn search_registry<'a>(idx: &'a RegistryIndex, query: &str) -> Vec<(&'a str, &'a RegistrySkillEntry)> {
    idx.search(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn cache_dir_path() {
        let d = tempdir().unwrap();
        assert!(registry_cache_dir(d.path()).ends_with("cache/registry"));
    }

    #[test]
    fn load_index_from_fs() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("index.yaml"), r#"
skills:
  test:
    latest: 1.0.0
    description: d
    publisher: human:t
    category: context
    tags: []
    content_sha256: "a"
"#).unwrap();
        let idx = load_index(d.path()).unwrap();
        assert_eq!(idx.skills.len(), 1);
    }

    #[test]
    fn skill_yaml_path_matches_spec() {
        let d = tempdir().unwrap();
        let p = skill_yaml_path(d.path(), "my-skill", "1.2.3");
        assert!(p.ends_with("skills/my-skill/versions/1.2.3.yaml"));
    }
}
