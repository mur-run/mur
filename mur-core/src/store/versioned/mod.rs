//! `VersionedYamlStore` — git-backed versioned pattern store (E1 W1).
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

pub mod agent;
mod agent_index;
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
            std::fs::create_dir_all(root.join(sub)).with_context(|| format!("mkdir {sub}"))?;
        }
        let knowledge_repo =
            git_ops::open_or_init_repo(root, KNOWLEDGE_GITIGNORE, "init: knowledge layer")?;
        let index = VersionIndex::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            knowledge_repo,
            index,
        })
    }

    /// Open an existing store. Returns an error if `root/.git` is absent.
    pub fn open(root: &Path) -> Result<Self> {
        let knowledge_repo = Repository::open(root)
            .with_context(|| format!("open knowledge repo at {}", root.display()))?;
        let index = VersionIndex::load(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            knowledge_repo,
            index,
        })
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

        // Serialize without version (version=0 is skipped by skip_serializing_if).
        let yaml = serde_yaml::to_string(pattern).with_context(|| format!("serialize {name}"))?;

        // No-op fast path: strip version/revision from existing before comparing.
        if pattern_abs.exists() {
            let existing = std::fs::read_to_string(&pattern_abs)?;
            if strip_version_meta(&existing) == yaml {
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

        // Write schema-3 version field into the YAML before committing.
        let new_v = prev_v + 1;
        let mut versioned = pattern.clone();
        versioned.version = new_v;
        let versioned_yaml = serde_yaml::to_string(&versioned)
            .with_context(|| format!("serialize {name} v{new_v}"))?;
        let tmp = pattern_abs.with_extension("yaml.tmp");
        std::fs::write(&tmp, &versioned_yaml)?;
        std::fs::rename(&tmp, &pattern_abs)?;

        let ts = chrono::Utc::now().to_rfc3339();
        let msg = format!("pattern({name}): v{new_v} {reason}");
        let sha = git_ops::commit_paths(&self.knowledge_repo, &paths_to_stage, &msg)?;

        let head = git_ops::head_sha(&self.knowledge_repo)?;
        self.index.append_version(name, &sha, reason, &head, &ts);
        self.index.save(&self.root)?;

        Ok(PatternRevision {
            name: name.clone(),
            version: new_v,
            sha,
        })
    }

    #[allow(dead_code)]
    pub fn read_pattern(&self, name: &str) -> Result<Option<Pattern>> {
        let path = self.root.join("patterns").join(format!("{name}.yaml"));
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(Some(
            serde_yaml::from_str(&content).with_context(|| format!("parse {name}"))?,
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
    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stage and commit a pattern file that has **already been written to disk**
    /// by `YamlStore::save()`. Used by the write path gate so that plain saves
    /// are automatically versioned when the knowledge git repo is active.
    ///
    /// No-op fast path: compares the new YAML against the HEAD blob (not the
    /// file) to detect whether anything actually changed.
    pub fn commit_existing_pattern(
        &mut self,
        pattern: &Pattern,
        reason: &str,
    ) -> Result<PatternRevision> {
        let name = &pattern.name;
        let pattern_rel = PathBuf::from("patterns").join(format!("{name}.yaml"));
        let pattern_abs = self.root.join(&pattern_rel);

        // Serialize the in-memory pattern (version=0, omitted by skip_serializing_if).
        let new_yaml =
            serde_yaml::to_string(pattern).with_context(|| format!("serialize {name}"))?;

        // Compare against HEAD blob with version/revision lines stripped so the
        // no-op check is not confused by schema-3 metadata added by this method.
        let head_yaml = self.read_head_blob_str(&pattern_rel).unwrap_or_default();
        if strip_version_meta(&head_yaml) == new_yaml {
            return Ok(PatternRevision {
                name: name.clone(),
                version: self.index.current_version_of(name),
                sha: git_ops::head_sha(&self.knowledge_repo)?,
            });
        }

        let mut paths_to_stage = vec![pattern_rel.clone()];

        // Archive the HEAD version before committing the new one.
        // The file on disk is already new content, so we copy from HEAD blob.
        let prev_v = self.index.current_version_of(name);
        if prev_v > 0 && !head_yaml.is_empty() {
            let archive_dir = self.root.join("archive/patterns").join(name);
            std::fs::create_dir_all(&archive_dir)?;
            let archive_rel = PathBuf::from("archive/patterns")
                .join(name)
                .join(format!("v{prev_v}.yaml"));
            std::fs::write(self.root.join(&archive_rel), &head_yaml)?;
            paths_to_stage.push(archive_rel);
        }

        let new_v = prev_v + 1;
        let ts = chrono::Utc::now().to_rfc3339();
        let msg = format!("pattern({name}): v{new_v} {reason}");

        // Write schema-3 version field into the on-disk file before committing
        // so the committed YAML contains version metadata (FIN-1: explicit path).
        let mut versioned = pattern.clone();
        versioned.version = new_v;
        let versioned_yaml = serde_yaml::to_string(&versioned)
            .with_context(|| format!("serialize {name} v{new_v}"))?;
        let tmp = pattern_abs.with_extension("yaml.tmp");
        std::fs::write(&tmp, &versioned_yaml)?;
        std::fs::rename(&tmp, &pattern_abs)?;

        let sha = git_ops::commit_paths(&self.knowledge_repo, &paths_to_stage, &msg)?;

        let head = git_ops::head_sha(&self.knowledge_repo)?;
        self.index.append_version(name, &sha, reason, &head, &ts);
        self.index.save(&self.root)?;

        Ok(PatternRevision {
            name: name.clone(),
            version: new_v,
            sha,
        })
    }

    /// Stage and commit all existing pattern YAML files in a single bootstrap
    /// commit. Skips patterns already identical to their HEAD blob.
    ///
    /// Used by `mur reindex --bootstrap` to import existing patterns into
    /// git history in one operation.
    pub fn bootstrap_all(&mut self) -> Result<usize> {
        let patterns_dir = self.root.join("patterns");
        if !patterns_dir.exists() {
            return Ok(0);
        }

        let head_tree = self
            .knowledge_repo
            .head()
            .ok()
            .and_then(|r| r.peel_to_tree().ok());

        let mut paths: Vec<PathBuf> = vec![];
        for entry in std::fs::read_dir(&patterns_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let rel = match path.strip_prefix(&self.root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            // Skip patterns already committed with identical content
            if let Some(tree) = &head_tree
                && let Ok(te) = tree.get_path(&rel)
                && let Ok(blob) = self.knowledge_repo.find_blob(te.id())
            {
                let on_disk = std::fs::read(&path).unwrap_or_default();
                if blob.content() == on_disk.as_slice() {
                    continue;
                }
            }
            paths.push(rel);
        }

        if paths.is_empty() {
            return Ok(0);
        }

        let count = paths.len();
        let ts = chrono::Utc::now().to_rfc3339();
        let msg = "bootstrap: import existing patterns";
        let sha = git_ops::commit_paths(&self.knowledge_repo, &paths, msg)?;
        let head = git_ops::head_sha(&self.knowledge_repo)?;

        for path_rel in &paths {
            if let Some(name) = path_rel.file_stem().and_then(|s| s.to_str()) {
                self.index
                    .append_version(name, &sha, "bootstrap: import existing", &head, &ts);
            }
        }
        self.index.save(&self.root)?;

        Ok(count)
    }

    fn read_head_blob_str(&self, rel: &Path) -> Result<String> {
        let tree = self.knowledge_repo.head()?.peel_to_tree()?;
        let entry = tree
            .get_path(rel)
            .with_context(|| format!("path {} not in HEAD", rel.display()))?;
        let blob = self.knowledge_repo.find_blob(entry.id())?;
        Ok(String::from_utf8_lossy(blob.content()).into_owned())
    }
}

/// Strip schema-3 `version:` and `revision:` lines from a YAML string so
/// no-op detection compares only semantic content, not version metadata.
fn strip_version_meta(yaml: &str) -> String {
    let mut result = yaml
        .lines()
        .filter(|l| !l.starts_with("version:") && !l.starts_with("revision:"))
        .collect::<Vec<_>>()
        .join("\n");
    // Preserve trailing newline that serde_yaml always produces.
    if yaml.ends_with('\n') {
        result.push('\n');
    }
    result
}
