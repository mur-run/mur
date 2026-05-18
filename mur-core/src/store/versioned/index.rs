//! `.mur-versions.yaml` — load-bearing per-pattern history index (FIN-3).
// Not yet wired into the CLI — remove when integrated (E1-W3).
#![allow(dead_code)]
//!
//! Maintained on every `save_pattern` call (O(1) append). `history()`
//! reads directly from this index instead of walking git log — the live
//! walk took 1.94s at 3000 commits in spike R2 (target: < 100ms).
//!
//! `rebuild_from_git` is the O(total-commits) recovery path; it runs only
//! on `mur internals rebuild-index` or after external git surgery.

use anyhow::Result;
use git2::Repository;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::git_ops;

pub const SCHEMA_VERSION: u32 = 3;
pub const INDEX_FILE: &str = ".mur-versions.yaml";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct VersionIndex {
    pub schema_version: u32,
    /// Full SHA of the knowledge repo HEAD at last index write.
    pub knowledge_head: String,
    #[serde(default)]
    pub patterns: BTreeMap<String, PatternIndex>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PatternIndex {
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

impl VersionIndex {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(INDEX_FILE);
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

    /// Atomic save (temp → rename) so a crash mid-write leaves the previous
    /// index intact.
    pub fn save(&self, root: &Path) -> Result<()> {
        let tmp = root.join(".mur-versions.yaml.tmp");
        let dest = root.join(INDEX_FILE);
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, dest)?;
        Ok(())
    }

    /// Append one version entry and advance `current_version`. O(1).
    /// Returns the new version number.
    pub fn append_version(
        &mut self,
        name: &str,
        sha: &str,
        reason: &str,
        head: &str,
        ts: &str,
    ) -> u32 {
        self.knowledge_head = head.to_string();
        let entry = self.patterns.entry(name.to_string()).or_default();
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

    pub fn current_version_of(&self, name: &str) -> u32 {
        self.patterns.get(name).map_or(0, |p| p.current_version)
    }

    /// Rebuild from git commit messages — O(total commits). Used only by
    /// `mur internals rebuild-index` and recovery paths.
    ///
    /// Commit message format: `"pattern(<name>): v<N> <reason>"`
    pub fn rebuild_from_git(repo: &Repository) -> Result<Self> {
        let head = git_ops::head_sha(repo)?;
        if head.is_empty() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                knowledge_head: head,
                ..Default::default()
            });
        }

        let mut per_pattern: BTreeMap<String, Vec<VersionEntry>> = BTreeMap::new();
        let mut walker = repo.revwalk()?;
        walker.push_head()?;

        for oid_result in walker {
            let oid = oid_result?;
            let commit = repo.find_commit(oid)?;
            let msg = commit.message().unwrap_or("").lines().next().unwrap_or("");
            if let Some((name, v, reason)) = parse_pattern_commit(msg) {
                let ts = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
                    .map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339())
                    .unwrap_or_default();
                per_pattern.entry(name).or_default().push(VersionEntry {
                    v,
                    sha: git_ops::short_sha(&oid),
                    ts,
                    reason,
                });
            }
        }

        let mut idx = Self {
            schema_version: SCHEMA_VERSION,
            knowledge_head: head,
            patterns: BTreeMap::new(),
        };

        for (name, mut entries) in per_pattern {
            // revwalk is newest-first; sort ascending by v for canonical order
            entries.sort_by_key(|e| e.v);
            let current_version = entries.last().map_or(0, |e| e.v);
            idx.patterns.insert(
                name,
                PatternIndex {
                    versions: entries,
                    current_version,
                },
            );
        }

        Ok(idx)
    }
}

/// Parse `"pattern(<name>): v<N> <reason>"` → `(name, v, reason)`.
fn parse_pattern_commit(msg: &str) -> Option<(String, u32, String)> {
    let msg = msg.strip_prefix("pattern(")?;
    let (name, rest) = msg.split_once("): v")?;
    let (v_str, reason) = rest.split_once(' ')?;
    let v: u32 = v_str.parse().ok()?;
    Some((name.to_string(), v, reason.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pattern_commit_valid() {
        let (name, v, reason) =
            parse_pattern_commit("pattern(rust-error-handling): v3 refine").unwrap();
        assert_eq!(name, "rust-error-handling");
        assert_eq!(v, 3);
        assert_eq!(reason, "refine");
    }

    #[test]
    fn parse_pattern_commit_rollback() {
        let (name, v, reason) = parse_pattern_commit("pattern(foo): v4 rollback to v2").unwrap();
        assert_eq!(v, 4);
        assert_eq!(reason, "rollback to v2");
        assert_eq!(name, "foo");
    }

    #[test]
    fn parse_pattern_commit_non_pattern() {
        assert!(parse_pattern_commit("init: knowledge layer").is_none());
        assert!(parse_pattern_commit("workflow(foo): v1 init").is_none());
    }
}
