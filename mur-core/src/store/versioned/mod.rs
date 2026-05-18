//! `VersionedYamlStore` — git-backed versioned pattern store (E1 W1).
// Not yet wired into the CLI — remove when integrated (E1-W3).
#![allow(dead_code)]
//!
//! Wraps `~/.mur/` as a knowledge-layer git repository. Every
//! `save_pattern` call produces exactly one commit and an O(1) index
//! append. `history()` reads the index directly (< 50ms target).
//!
//! ADR-0001 compliance:
//!   FIN-1 — `commit_paths` stages only explicit paths (no `add_all`)
//!   FIN-2 — version derivation O(1) via archive dir count
//!   FIN-3 — `.mur-versions.yaml` is load-bearing; `history()` reads it
//!   FIN-4 — knowledge.gitignore uses bare patterns, no `*/` anchors

mod git_ops;
mod index;

use anyhow::{Context, Result, anyhow};
use git2::Repository;
use index::VersionIndex;
use mur_common::pattern::Pattern;
use std::path::{Path, PathBuf};

const KNOWLEDGE_GITIGNORE: &str = include_str!("knowledge.gitignore");

pub struct VersionedYamlStore {
    root: PathBuf,
    knowledge_repo: Repository,
    index: VersionIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternRevision {
    pub name: String,
    pub version: u32,
    /// 12-char short SHA of the git commit for this version.
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub version: u32,
    pub sha: String,
    /// Unix timestamp (seconds since epoch).
    pub timestamp: i64,
    pub reason: String,
}

impl VersionedYamlStore {
    /// Initialise (or re-open) a versioned store at `root`. Creates the
    /// knowledge git repo and required subdirectories on first call.
    pub fn init(root: &Path) -> Result<Self> {
        for sub in ["patterns", "archive/patterns", "workflows"] {
            std::fs::create_dir_all(root.join(sub))
                .with_context(|| format!("mkdir {sub}"))?;
        }
        let knowledge_repo = git_ops::open_or_init_repo(
            root,
            KNOWLEDGE_GITIGNORE,
            "init: knowledge layer",
        )?;
        let index = VersionIndex::load(root)?;
        Ok(Self { root: root.to_path_buf(), knowledge_repo, index })
    }

    /// Open an existing store. Returns an error if `root/.git` is absent.
    pub fn open(root: &Path) -> Result<Self> {
        let knowledge_repo = Repository::open(root)
            .with_context(|| format!("open knowledge repo at {}", root.display()))?;
        let index = VersionIndex::load(root)?;
        Ok(Self { root: root.to_path_buf(), knowledge_repo, index })
    }

    /// Write `pattern` to disk and commit. Returns a `PatternRevision`
    /// describing the new version.
    ///
    /// No-op fast path: if the serialised YAML is byte-identical to what
    /// is on disk, returns the current revision without a new commit.
    pub fn save_pattern(&mut self, pattern: &Pattern, reason: &str) -> Result<PatternRevision> {
        let name = &pattern.name;
        let pattern_rel = PathBuf::from("patterns").join(format!("{name}.yaml"));
        let pattern_abs = self.root.join(&pattern_rel);

        let yaml = serde_yaml::to_string(pattern)
            .with_context(|| format!("serialize {name}"))?;

        // No-op fast path
        if pattern_abs.exists() {
            let existing = std::fs::read_to_string(&pattern_abs)?;
            if existing == yaml {
                return Ok(PatternRevision {
                    name: name.clone(),
                    version: self.index.current_version_of(name),
                    sha: git_ops::head_sha(&self.knowledge_repo)?,
                });
            }
        }

        let mut paths_to_stage = vec![pattern_rel.clone()];

        // Archive the current version before overwriting (FIN-2: uses
        // current_version from index — O(1), not a git walk).
        let prev_v = self.index.current_version_of(name);
        if prev_v > 0 && pattern_abs.exists() {
            let archive_dir = self.root.join("archive/patterns").join(name);
            std::fs::create_dir_all(&archive_dir)?;
            let archive_rel = PathBuf::from("archive/patterns")
                .join(name)
                .join(format!("v{prev_v}.yaml"));
            std::fs::copy(&pattern_abs, self.root.join(&archive_rel))?;
            paths_to_stage.push(archive_rel);
        }

        // Atomic write: temp → rename
        let tmp = pattern_abs.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml)?;
        std::fs::rename(&tmp, &pattern_abs)?;

        let new_v = prev_v + 1;
        let ts = chrono::Utc::now().to_rfc3339();
        let msg = format!("pattern({name}): v{new_v} {reason}");
        let sha = git_ops::commit_paths(&self.knowledge_repo, &paths_to_stage, &msg)?;

        let head = git_ops::head_sha(&self.knowledge_repo)?;
        self.index.append_version(name, &sha, reason, &head, &ts);
        self.index.save(&self.root)?;

        Ok(PatternRevision { name: name.clone(), version: new_v, sha })
    }

    pub fn read_pattern(&self, name: &str) -> Result<Option<Pattern>> {
        let path = self.root.join("patterns").join(format!("{name}.yaml"));
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(Some(
            serde_yaml::from_str(&content)
                .with_context(|| format!("parse {name}"))?,
        ))
    }

    /// Current version from the index. O(1).
    pub fn current_version(&self, name: &str) -> u32 {
        self.index.current_version_of(name)
    }

    /// Full version history for `name` read from the index (FIN-3).
    pub fn history(&self, name: &str) -> Result<Vec<HistoryEntry>> {
        let pi = match self.index.patterns.get(name) {
            Some(p) => p,
            None => return Ok(vec![]),
        };
        Ok(pi
            .versions
            .iter()
            .map(|e| HistoryEntry {
                version: e.v,
                sha: e.sha.clone(),
                timestamp: chrono::DateTime::parse_from_rfc3339(&e.ts)
                    .map(|dt| dt.timestamp())
                    .unwrap_or(0),
                reason: e.reason.clone(),
            })
            .collect())
    }

    /// Roll back `name` to `to_version` by re-applying the archived YAML
    /// as a new commit. No history rewriting.
    pub fn rollback_pattern(&mut self, name: &str, to_version: u32) -> Result<PatternRevision> {
        let archive = self
            .root
            .join("archive/patterns")
            .join(name)
            .join(format!("v{to_version}.yaml"));
        if !archive.exists() {
            return Err(anyhow!("no archived v{to_version} for pattern '{name}'"));
        }
        let content = std::fs::read_to_string(&archive)?;
        let pattern: Pattern = serde_yaml::from_str(&content)
            .with_context(|| format!("parse archive v{to_version} of '{name}'"))?;
        self.save_pattern(&pattern, &format!("rollback to v{to_version}"))
    }

    /// Returns `true` if the on-disk HEAD differs from the HEAD recorded in
    /// the index — indicating external git surgery since the last save.
    pub fn detect_external_change(&self) -> Result<bool> {
        let head = git_ops::head_sha(&self.knowledge_repo)?;
        Ok(!head.is_empty() && self.index.knowledge_head != head)
    }

    /// Rebuild `.mur-versions.yaml` by walking the full git log.
    /// O(total commits) — use only for recovery or first migration.
    pub fn rebuild_index(&mut self) -> Result<()> {
        self.index = VersionIndex::rebuild_from_git(&self.knowledge_repo)?;
        self.index.save(&self.root)?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
