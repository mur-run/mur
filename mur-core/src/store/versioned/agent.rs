//! `VersionedAgentStore` — git-backed execution-layer store (E1 W2).
// Not yet wired into the CLI — remove when integrated (E1-W3).
#![allow(dead_code)]
//!
//! Manages `~/.mur/agents/.git` (execution layer). Profile + skills
//! for every agent are versioned here, independently from the knowledge
//! layer in `~/.mur/.git`.
//!
//! ADR-0001 compliance: same FIN-1/2/3/4 rules as VersionedYamlStore.

use anyhow::{Context, Result, anyhow};
use git2::Repository;
use std::path::{Path, PathBuf};

use super::agent_index::AgentVersionIndex;
use super::git_ops;

const AGENTS_GITIGNORE: &str = include_str!("agents.gitignore");

pub struct VersionedAgentStore {
    /// `~/.mur/agents/`
    root: PathBuf,
    agents_repo: Repository,
    index: AgentVersionIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRevision {
    pub name: String,
    pub version: u32,
    /// 12-char short SHA of the agents-repo commit for this version.
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct AgentHistoryEntry {
    pub version: u32,
    pub sha: String,
    pub timestamp: i64,
    pub reason: String,
}

/// Outcome of [`VersionedAgentStore::repair`].
#[derive(Debug, Clone, Default)]
pub struct RepairReport {
    pub recovered: bool,
    pub agents_recommitted: usize,
}

impl VersionedAgentStore {
    /// Initialise (or re-open) the agents store. Creates `.git` on first call.
    pub fn init(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("mkdir agents dir {}", root.display()))?;
        let agents_repo = git_ops::open_or_init_repo(root, AGENTS_GITIGNORE, "init: agents layer")?;
        let index = AgentVersionIndex::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            agents_repo,
            index,
        })
    }

    /// Open an existing store. Returns an error if `.git` is absent.
    pub fn open(root: &Path) -> Result<Self> {
        let agents_repo = Repository::open(root)
            .with_context(|| format!("open agents repo at {}", root.display()))?;
        let index = AgentVersionIndex::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            agents_repo,
            index,
        })
    }

    /// Re-init agents repo when `.git` is missing or broken (split-brain
    /// recovery). Existing agent dirs are preserved and committed in one
    /// "repair" commit. Static fn because `open` would fail at this point.
    pub fn repair(root: &Path) -> Result<RepairReport> {
        std::fs::create_dir_all(root)?;
        let git_dir = root.join(".git");
        let openable = git_dir.exists() && Repository::open(root).is_ok();
        if openable {
            return Ok(RepairReport::default());
        }

        if git_dir.exists() {
            std::fs::remove_dir_all(&git_dir)
                .with_context(|| format!("remove broken {}", git_dir.display()))?;
        }

        let repo = Repository::init(root)?;
        let gi_path = root.join(".gitignore");
        if !gi_path.exists() {
            std::fs::write(&gi_path, AGENTS_GITIGNORE)?;
        }

        let agents_recommitted = count_agent_dirs(root);
        let sig = git_ops::signature()?;
        let mut index = repo.index()?;
        index.add_path(Path::new(".gitignore"))?;
        // Recovery path only — add_all is allowed here (FIN-1 exception)
        index.add_all(["."], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        {
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "repair: recover from missing/broken agents/.git",
                &tree,
                &[],
            )?;
        }

        Ok(RepairReport {
            recovered: true,
            agents_recommitted,
        })
    }

    /// Write `profile_yaml` to `<agent>/profile.yaml` and commit.
    ///
    /// No-op fast path: returns current revision if content is unchanged.
    pub fn save_profile(
        &mut self,
        agent: &str,
        profile_yaml: &str,
        reason: &str,
    ) -> Result<AgentRevision> {
        let profile_rel = PathBuf::from(agent).join("profile.yaml");
        let profile_abs = self.root.join(&profile_rel);

        // No-op fast path
        if profile_abs.exists() {
            let existing = std::fs::read_to_string(&profile_abs)?;
            if existing == profile_yaml {
                return Ok(AgentRevision {
                    name: agent.to_string(),
                    version: self.index.current_version_of(agent),
                    sha: git_ops::head_sha(&self.agents_repo)?,
                });
            }
        }

        let mut paths_to_stage = vec![profile_rel.clone()];

        // Archive previous version (O(1) via index — FIN-2)
        let prev_v = self.index.current_version_of(agent);
        if prev_v > 0 && profile_abs.exists() {
            let archive_dir = self.root.join(agent).join("archive");
            std::fs::create_dir_all(&archive_dir)?;
            let archive_rel = PathBuf::from(agent)
                .join("archive")
                .join(format!("v{prev_v}.yaml"));
            std::fs::copy(&profile_abs, self.root.join(&archive_rel))?;
            paths_to_stage.push(archive_rel);
        }

        std::fs::create_dir_all(profile_abs.parent().unwrap())?;
        // Atomic write
        let tmp = profile_abs.with_extension("yaml.tmp");
        std::fs::write(&tmp, profile_yaml)?;
        std::fs::rename(&tmp, &profile_abs)?;

        let new_v = prev_v + 1;
        let ts = chrono::Utc::now().to_rfc3339();
        let msg = format!("profile({agent}): v{new_v} {reason}");
        let sha = git_ops::commit_paths(&self.agents_repo, &paths_to_stage, &msg)?;

        let head = git_ops::head_sha(&self.agents_repo)?;
        self.index.append_version(agent, &sha, reason, &head, &ts);
        self.index.save(&self.root)?;

        Ok(AgentRevision {
            name: agent.to_string(),
            version: new_v,
            sha,
        })
    }

    pub fn read_profile(&self, agent: &str) -> Result<Option<String>> {
        let path = self.root.join(agent).join("profile.yaml");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(path)?))
    }

    /// Write a skill file to `<agent>/skills/<skill>.md` and commit.
    pub fn save_skill(
        &mut self,
        agent: &str,
        skill: &str,
        content: &str,
        reason: &str,
    ) -> Result<AgentRevision> {
        let skills_dir = self.root.join(agent).join("skills");
        std::fs::create_dir_all(&skills_dir)?;
        let skill_rel = PathBuf::from(agent)
            .join("skills")
            .join(format!("{skill}.md"));
        let skill_abs = self.root.join(&skill_rel);

        // No-op fast path
        if skill_abs.exists() && std::fs::read_to_string(&skill_abs)? == content {
            return Ok(AgentRevision {
                name: agent.to_string(),
                version: self.index.current_version_of(agent),
                sha: git_ops::head_sha(&self.agents_repo)?,
            });
        }

        let tmp = skill_abs.with_extension("md.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &skill_abs)?;

        let new_v = self.index.current_version_of(agent) + 1;
        let ts = chrono::Utc::now().to_rfc3339();
        let msg = format!("profile({agent}): v{new_v} {reason}");
        let sha = git_ops::commit_paths(&self.agents_repo, &[skill_rel], &msg)?;

        let head = git_ops::head_sha(&self.agents_repo)?;
        self.index.append_version(agent, &sha, reason, &head, &ts);
        self.index.save(&self.root)?;

        Ok(AgentRevision {
            name: agent.to_string(),
            version: new_v,
            sha,
        })
    }

    pub fn read_skill(&self, agent: &str, skill: &str) -> Result<Option<String>> {
        let path = self
            .root
            .join(agent)
            .join("skills")
            .join(format!("{skill}.md"));
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(path)?))
    }

    /// History from the index (FIN-3 — O(K) where K = revisions of this agent).
    pub fn history(&self, agent: &str) -> Result<Vec<AgentHistoryEntry>> {
        let ai = match self.index.agents.get(agent) {
            Some(a) => a,
            None => return Ok(vec![]),
        };
        Ok(ai
            .versions
            .iter()
            .map(|e| AgentHistoryEntry {
                version: e.v,
                sha: e.sha.clone(),
                timestamp: chrono::DateTime::parse_from_rfc3339(&e.ts)
                    .map(|dt| dt.timestamp())
                    .unwrap_or(0),
                reason: e.reason.clone(),
            })
            .collect())
    }

    pub fn current_version(&self, agent: &str) -> u32 {
        self.index.current_version_of(agent)
    }

    /// Roll back agent profile to `to_version` as a new commit.
    pub fn rollback_profile(&mut self, agent: &str, to_version: u32) -> Result<AgentRevision> {
        let archive = self
            .root
            .join(agent)
            .join("archive")
            .join(format!("v{to_version}.yaml"));
        if !archive.exists() {
            return Err(anyhow!("no archived v{to_version} for agent '{agent}'"));
        }
        let content = std::fs::read_to_string(&archive)?;
        self.save_profile(agent, &content, &format!("rollback to v{to_version}"))
    }

    /// Export a single agent's git history as a portable bundle file.
    ///
    /// Uses `git subtree split --prefix=<agent>` to produce a branch
    /// containing only commits that touched that agent's directory, then
    /// bundles it with `git bundle create`.
    ///
    /// Requires git ≥ 2.37 with `subtree` contrib command available.
    pub fn export_history(&self, agent: &str, bundle_path: &Path) -> Result<()> {
        let root = &self.root;
        let branch = format!("export-{agent}");

        // 1. subtree split — isolate agent's history into a temp branch
        let status = std::process::Command::new("git")
            .args([
                "-C",
                root.to_str().unwrap(),
                "subtree",
                "split",
                "--prefix",
                agent,
                "-b",
                &branch,
            ])
            .status()
            .with_context(|| "git subtree split failed — is git-subtree installed?")?;
        if !status.success() {
            return Err(anyhow!(
                "git subtree split returned non-zero for agent '{agent}'"
            ));
        }

        // 2. bundle the split branch
        let bundle_str = bundle_path.to_str().unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-C",
                root.to_str().unwrap(),
                "bundle",
                "create",
                bundle_str,
                &branch,
            ])
            .status()
            .with_context(|| "git bundle create failed")?;
        if !status.success() {
            return Err(anyhow!("git bundle create returned non-zero"));
        }

        // 3. clean up temp branch
        let _ = std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "branch", "-D", &branch])
            .status();

        Ok(())
    }

    /// Returns `true` if on-disk HEAD differs from the index — external git surgery.
    pub fn detect_external_change(&self) -> Result<bool> {
        let head = git_ops::head_sha(&self.agents_repo)?;
        Ok(!head.is_empty() && self.index.agents_head != head)
    }

    /// Rebuild index from git commit messages. O(total commits) — recovery only.
    pub fn rebuild_index(&mut self) -> Result<()> {
        self.index = AgentVersionIndex::rebuild_from_git(&self.agents_repo)?;
        self.index.save(&self.root)?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn count_agent_dirs(agents_dir: &Path) -> usize {
    std::fs::read_dir(agents_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
        .unwrap_or(0)
}
