//! `.mur-agents-versions.yaml` — load-bearing per-agent history index.
// Not yet wired into the CLI — remove when integrated (E1-W3).
#![allow(dead_code)]
//!
//! Mirrors `index.rs` for the execution layer (`~/.mur/agents/.git`).
//! Same FIN-3 invariant: `history()` reads here, never from live git walk.

use anyhow::Result;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::git_ops;

pub const SCHEMA_VERSION: u32 = 3;
pub const INDEX_FILE: &str = ".mur-agents-versions.yaml";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AgentVersionIndex {
    pub schema_version: u32,
    /// Full SHA of the agents repo HEAD at last index write.
    pub agents_head: String,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentIndex>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AgentIndex {
    pub versions: Vec<VersionEntry>,
    pub current_version: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VersionEntry {
    pub v: u32,
    pub sha: String,
    pub ts: String,
    pub reason: String,
}

impl AgentVersionIndex {
    pub fn load(agents_dir: &Path) -> Result<Self> {
        let path = agents_dir.join(INDEX_FILE);
        if !path.exists() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                ..Default::default()
            });
        }
        let content = std::fs::read_to_string(&path)?;
        let mut idx: Self = serde_yaml::from_str(&content)?;
        idx.schema_version = SCHEMA_VERSION;
        Ok(idx)
    }

    /// Atomic save (temp → rename).
    pub fn save(&self, agents_dir: &Path) -> Result<()> {
        let tmp = agents_dir.join(".mur-agents-versions.yaml.tmp");
        let dest = agents_dir.join(INDEX_FILE);
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    /// O(1) append. Returns new version number.
    pub fn append_version(
        &mut self,
        agent: &str,
        sha: &str,
        reason: &str,
        head: &str,
        ts: &str,
    ) -> u32 {
        self.agents_head = head.to_string();
        let entry = self.agents.entry(agent.to_string()).or_default();
        let v = entry.current_version + 1;
        entry.versions.push(VersionEntry {
            v,
            sha: sha.to_string(),
            ts: ts.to_string(),
            reason: reason.to_string(),
        });
        entry.current_version = v;
        v
    }

    pub fn current_version_of(&self, agent: &str) -> u32 {
        self.agents.get(agent).map_or(0, |a| a.current_version)
    }

    /// Rebuild from git commit messages. O(total commits) — recovery only.
    ///
    /// Commit message format: `"profile(<name>): v<N> <reason>"`
    pub fn rebuild_from_git(repo: &Repository) -> Result<Self> {
        let head = git_ops::head_sha(repo)?;
        if head.is_empty() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                agents_head: head,
                ..Default::default()
            });
        }

        let mut per_agent: BTreeMap<String, Vec<VersionEntry>> = BTreeMap::new();
        let mut walker = repo.revwalk()?;
        walker.push_head()?;

        for oid_result in walker {
            let oid = oid_result?;
            let commit = repo.find_commit(oid)?;
            let msg = commit
                .message()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            if let Some((name, v, reason)) = parse_profile_commit(msg) {
                let ts = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
                    .map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339())
                    .unwrap_or_default();
                per_agent
                    .entry(name)
                    .or_default()
                    .push(VersionEntry { v, sha: git_ops::short_sha(&oid), ts, reason });
            }
        }

        let mut idx = Self {
            schema_version: SCHEMA_VERSION,
            agents_head: head,
            agents: BTreeMap::new(),
        };

        for (name, mut entries) in per_agent {
            entries.sort_by_key(|e| e.v);
            let current_version = entries.last().map_or(0, |e| e.v);
            idx.agents.insert(name, AgentIndex { versions: entries, current_version });
        }

        Ok(idx)
    }
}

/// Parse `"profile(<name>): v<N> <reason>"` → `(name, v, reason)`.
pub fn parse_profile_commit(msg: &str) -> Option<(String, u32, String)> {
    let msg = msg.strip_prefix("profile(")?;
    let (name, rest) = msg.split_once("): v")?;
    let (v_str, reason) = rest.split_once(' ')?;
    let v: u32 = v_str.parse().ok()?;
    Some((name.to_string(), v, reason.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_commit_valid() {
        let (name, v, reason) =
            parse_profile_commit("profile(my-agent): v2 update skills").unwrap();
        assert_eq!(name, "my-agent");
        assert_eq!(v, 2);
        assert_eq!(reason, "update skills");
    }

    #[test]
    fn parse_profile_commit_non_profile() {
        assert!(parse_profile_commit("init: agents layer").is_none());
        assert!(parse_profile_commit("pattern(foo): v1 init").is_none());
    }
}
