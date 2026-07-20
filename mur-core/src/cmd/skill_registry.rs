//! Skill registry client — shallow clone via git, load index, search.
//!
//! Default registry: https://github.com/mur-run/skill-registry.git
//! Cached at: ~/.mur/cache/registry/ (shallow clone, refreshed)

use anyhow::{Context, Result};
use mur_common::skill::registry::{RegistryIndex, RegistrySkillEntry};
use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_REGISTRY: &str = "https://github.com/mur-run/skill-registry.git";

pub fn registry_cache_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join("registry")
}

pub fn fetch_registry(mur_home: &Path, registry_url: &str) -> Result<PathBuf> {
    let cache_dir = registry_cache_dir(mur_home);
    git_clone_or_pull(registry_url, &cache_dir)?;
    Ok(cache_dir)
}

/// Shallow-clone `url` into `dest`, or `git pull --ff-only` if it already
/// exists. Shared by the skill registry and the addon network installer
/// (`mur agent addon import <agent> owner/repo`). A failed refresh of an
/// existing clone is a warning, not an error (we fall back to the cache).
pub fn git_clone_or_pull(url: &str, dest: &Path) -> Result<()> {
    if dest.join(".git").exists() {
        let status = Command::new("git")
            .args([
                "-C",
                &*dest.to_string_lossy(),
                "pull",
                "--depth=1",
                "--ff-only",
            ])
            .status()
            .map_err(|e| anyhow::anyhow!("git pull: {e}"))?;
        if !status.success() {
            eprintln!(
                "warning: refresh of {} failed, using cached",
                dest.display()
            );
        }
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let status = Command::new("git")
            .args(["clone", "--depth=1", url, &*dest.to_string_lossy()])
            .status()
            .map_err(|e| anyhow::anyhow!("git clone: {e}"))?;
        if !status.success() {
            anyhow::bail!("failed to clone {url}");
        }
    }
    Ok(())
}

/// Shallow-clone a single ref (branch or tag) of `url` into `dest`.
/// `--branch` accepts branch and tag names; arbitrary commit SHAs are not
/// supported (GitHub `tree` URLs use branch/tag names).
pub fn git_clone_ref(url: &str, git_ref: &str, dest: &Path) -> Result<()> {
    let status = Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "--branch",
            git_ref,
            url,
            &*dest.to_string_lossy(),
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("git clone: {e}"))?;
    if !status.success() {
        anyhow::bail!("git clone {url} (ref {git_ref}) failed");
    }
    Ok(())
}

pub fn load_index(registry_dir: &Path) -> Result<RegistryIndex> {
    let p = registry_dir.join("index.yaml");
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    RegistryIndex::from_yaml(&text).map_err(|e| anyhow::anyhow!("parse index: {e}"))
}

pub fn fetch_and_load(mur_home: &Path, url: &str) -> Result<(PathBuf, RegistryIndex)> {
    let dir = fetch_registry(mur_home, url)?;
    let idx = load_index(&dir)?;
    Ok((dir, idx))
}

pub fn skill_yaml_path(registry_dir: &Path, name: &str, version: &str) -> PathBuf {
    registry_dir
        .join("skills")
        .join(name)
        .join("versions")
        .join(format!("{version}.yaml"))
}

/// List the versions of `name` available in the registry cache.
/// Returns sorted ascending. Returns `Ok(vec![])` if the skill dir doesn't exist.
pub fn available_versions(registry_dir: &Path, name: &str) -> Result<Vec<Version>> {
    let dir = registry_dir.join("skills").join(name).join("versions");
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        let Some(stripped) = name_str.strip_suffix(".yaml") else {
            continue;
        };
        match Version::parse(stripped) {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!(
                file = %name_str,
                error = %e,
                "skipping non-semver version filename"
            ),
        }
    }
    out.sort();
    Ok(out)
}

pub fn search_registry<'a>(
    idx: &'a RegistryIndex,
    query: &str,
) -> Vec<(&'a str, &'a RegistrySkillEntry)> {
    idx.search(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn git(args: &[&str], cwd: &Path) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn git_clone_or_pull_clones_then_refreshes() {
        let tmp = tempdir().unwrap();
        // Build a source repo with one committed file.
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        git(&["init", "-q", "-b", "main"], &src);
        git(&["config", "user.email", "t@t"], &src);
        git(&["config", "user.name", "t"], &src);
        fs::write(src.join("plugin.json"), "{}").unwrap();
        git(&["add", "."], &src);
        git(&["commit", "-qm", "init"], &src);

        let dest = tmp.path().join("cache").join("addons").join("local-src");
        // First call clones (dest does not exist yet; parent is created).
        git_clone_or_pull(&src.to_string_lossy(), &dest).unwrap();
        assert!(dest.join("plugin.json").exists(), "clone produced the file");
        // Second call hits the pull branch and still succeeds.
        git_clone_or_pull(&src.to_string_lossy(), &dest).unwrap();
        assert!(dest.join("plugin.json").exists());
    }

    #[test]
    fn git_clone_ref_checks_out_named_branch() {
        use std::process::Command;
        let src = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str], cwd: &std::path::Path| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q", "-b", "main"], src.path());
        run(&["config", "user.email", "t@t"], src.path());
        run(&["config", "user.name", "t"], src.path());
        std::fs::write(src.path().join("f.txt"), "hi").unwrap();
        run(&["add", "."], src.path());
        run(&["commit", "-q", "-m", "c"], src.path());

        let dest = tempfile::TempDir::new().unwrap();
        let target = dest.path().join("clone");
        let url = format!("file://{}", src.path().display());
        super::git_clone_ref(&url, "main", &target).unwrap();
        assert!(target.join("f.txt").is_file());
    }

    #[test]
    fn cache_dir_path() {
        let d = tempdir().unwrap();
        assert!(registry_cache_dir(d.path()).ends_with("cache/registry"));
    }

    #[test]
    fn load_index_from_fs() {
        let d = tempdir().unwrap();
        fs::write(
            d.path().join("index.yaml"),
            r#"
skills:
  test:
    latest: 1.0.0
    description: d
    publisher: human:t
    category: context
    tags: []
    content_sha256: "a"
"#,
        )
        .unwrap();
        let idx = load_index(d.path()).unwrap();
        assert_eq!(idx.skills.len(), 1);
    }

    #[test]
    fn skill_yaml_path_matches_spec() {
        let d = tempdir().unwrap();
        let p = skill_yaml_path(d.path(), "my-skill", "1.2.3");
        assert!(p.ends_with("skills/my-skill/versions/1.2.3.yaml"));
    }

    #[test]
    fn available_versions_missing_dir() {
        let d = tempdir().unwrap();
        let vs = available_versions(d.path(), "nonexistent").unwrap();
        assert!(vs.is_empty());
    }

    #[test]
    fn available_versions_sorted() {
        let d = tempdir().unwrap();
        let versions_dir = d.path().join("skills/my-skill/versions");
        fs::create_dir_all(&versions_dir).unwrap();
        fs::write(versions_dir.join("1.0.0.yaml"), "").unwrap();
        fs::write(versions_dir.join("0.9.0.yaml"), "").unwrap();
        fs::write(versions_dir.join("2.0.0.yaml"), "").unwrap();
        let vs = available_versions(d.path(), "my-skill").unwrap();
        assert_eq!(
            vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            vec!["0.9.0", "1.0.0", "2.0.0"]
        );
    }

    #[test]
    fn available_versions_skips_non_semver() {
        let d = tempdir().unwrap();
        let versions_dir = d.path().join("skills/my-skill/versions");
        fs::create_dir_all(&versions_dir).unwrap();
        fs::write(versions_dir.join("1.0.0.yaml"), "").unwrap();
        fs::write(versions_dir.join("not-a-version.yaml"), "").unwrap();
        fs::write(versions_dir.join("2.0.0.yaml"), "").unwrap();
        let vs = available_versions(d.path(), "my-skill").unwrap();
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0].to_string(), "1.0.0");
        assert_eq!(vs[1].to_string(), "2.0.0");
    }
}
