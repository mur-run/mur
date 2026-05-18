//! E1 dual-git-repo versioned-store spike.
//!
//! This is a *minimum vertical slice* of the v2 spec §4 design — just enough
//! code to exercise the 6 core operations so we can measure the 8 risk
//! assumptions on day 2. NOT production code.
//!
//! Mapping to spec §4:
//! - `~/.mur/.git`      = knowledge_repo (patterns/, workflows/, archive/)
//! - `~/.mur/agents/.git` = agents_repo (per-agent profile/skills/perms)
//!
//! Simplifications vs production:
//! - Version derived from commit count, not embedded in YAML
//! - No `.mur-versions.yaml` cache (rebuilt every read)
//! - No atomic temp-rename (relying on git index atomicity)
//! - Patterns are opaque strings, not parsed
//! - No file locking — that's risk #3 to test, not to mitigate yet

use anyhow::{Context, Result, anyhow};
use git2::{IndexAddOption, Repository, Signature};
use std::path::{Path, PathBuf};

pub struct SpikeStore {
    root: PathBuf,
    knowledge_repo: Repository,
    agents_repo: Repository,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternRevision {
    pub name: String,
    pub version: u32,
    pub sha: String,
    pub archived_as: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub version: u32,
    pub sha: String,
    pub message: String,
    pub timestamp: i64,
}

/// Outcome of [`SpikeStore::repair_agents`] — used by R8 split-brain test.
#[derive(Debug, Clone, Default)]
pub struct RepairReport {
    /// True if the agents repo was actually re-initialised (missing or
    /// unopenable .git); false if no repair was needed.
    pub recovered: bool,
    /// Number of agent directories that got re-committed in the recovery
    /// commit (rough proxy for "how much state did we rescue").
    pub agents_recommitted: usize,
}

impl SpikeStore {
    /// Initialise both repos under `root`. Idempotent — calling on existing
    /// dirs re-opens.
    pub fn init(root: &Path) -> Result<Self> {
        for sub in ["patterns", "workflows", "archive/patterns", "agents"] {
            std::fs::create_dir_all(root.join(sub))
                .with_context(|| format!("mkdir {}", sub))?;
        }

        let knowledge_repo = open_or_init_repo(
            root,
            include_str!("knowledge.gitignore"),
            "init: knowledge layer",
        )?;
        let agents_repo = open_or_init_repo(
            &root.join("agents"),
            include_str!("agents.gitignore"),
            "init: agents layer",
        )?;

        Ok(Self {
            root: root.to_path_buf(),
            knowledge_repo,
            agents_repo,
        })
    }

    /// Open both repos under `root`. Strict — errors if either .git is
    /// missing or broken. For graceful recovery, use [`Self::repair_agents`]
    /// first.
    pub fn open(root: &Path) -> Result<Self> {
        let knowledge_repo = Repository::open(root)
            .with_context(|| format!("open knowledge repo at {}", root.display()))?;
        let agents_repo = Repository::open(root.join("agents")).with_context(|| {
            format!("open agents repo at {}/agents", root.display())
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            knowledge_repo,
            agents_repo,
        })
    }

    /// Re-init the agents repo when `agents/.git` is missing or broken.
    /// Existing files in `agents/<name>/` are preserved and committed
    /// in a single "repair" commit (history lost, current state intact).
    ///
    /// Static/associated fn because [`Self::open`] would fail at this point.
    pub fn repair_agents(root: &Path) -> Result<RepairReport> {
        let agents_dir = root.join("agents");
        std::fs::create_dir_all(&agents_dir)?;

        let git_dir = agents_dir.join(".git");
        let openable = git_dir.exists() && Repository::open(&agents_dir).is_ok();
        if openable {
            return Ok(RepairReport::default());
        }

        // Nuke residual broken .git if it exists but won't open
        if git_dir.exists() {
            std::fs::remove_dir_all(&git_dir).with_context(|| {
                format!("remove broken {}", git_dir.display())
            })?;
        }

        let repo = Repository::init(&agents_dir)?;
        let gi_path = agents_dir.join(".gitignore");
        if !gi_path.exists() {
            std::fs::write(&gi_path, include_str!("agents.gitignore"))?;
        }

        let agents_recommitted = count_agent_dirs(&agents_dir);

        let sig = signature()?;
        let mut index = repo.index()?;
        // .gitignore added first so subsequent add_all respects it
        index.add_path(Path::new(".gitignore"))?;
        index.add_all(["."], IndexAddOption::DEFAULT, None)?;
        index.write()?;
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

        Ok(RepairReport {
            recovered: true,
            agents_recommitted,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Re-open without re-initialising. Use after external git operations.
    pub fn reopen(&mut self) -> Result<()> {
        self.knowledge_repo = Repository::open(&self.root)?;
        self.agents_repo = Repository::open(self.root.join("agents"))?;
        Ok(())
    }

    /// Write `content` to `patterns/<name>.yaml`, archive previous version if
    /// content differs, commit. Returns the new revision.
    ///
    /// If content hash matches existing, no-op: returns current rev without
    /// commit (fast-path required by §4.2.3).
    pub fn save_pattern(
        &mut self,
        name: &str,
        content: &str,
        reason: &str,
    ) -> Result<PatternRevision> {
        let pattern_rel = PathBuf::from("patterns").join(format!("{name}.yaml"));
        let pattern_abs = self.root.join(&pattern_rel);

        let prev_version = self.current_pattern_version(name)?;
        let mut paths_to_stage: Vec<PathBuf> = vec![pattern_rel.clone()];
        let mut archived_as: Option<PathBuf> = None;

        if pattern_abs.exists() {
            let existing = std::fs::read_to_string(&pattern_abs)?;
            if existing == content {
                // No-op fast path
                let head_sha = self.head_sha(&self.knowledge_repo)?;
                return Ok(PatternRevision {
                    name: name.to_string(),
                    version: prev_version,
                    sha: head_sha,
                    archived_as: None,
                });
            }
            // Archive
            let archive_dir = PathBuf::from("archive/patterns").join(name);
            std::fs::create_dir_all(self.root.join(&archive_dir))?;
            let archive_rel = archive_dir.join(format!("v{prev_version}.yaml"));
            std::fs::copy(&pattern_abs, self.root.join(&archive_rel))?;
            paths_to_stage.push(archive_rel.clone());
            archived_as = Some(archive_rel);
        }

        let new_version = prev_version + 1;
        std::fs::write(&pattern_abs, content)?;

        let message = format!("pattern({name}): v{new_version} {reason}");
        let sha = commit_paths(&self.knowledge_repo, &paths_to_stage, &message)?;

        Ok(PatternRevision {
            name: name.to_string(),
            version: new_version,
            sha,
            archived_as,
        })
    }

    pub fn read_pattern(&self, name: &str) -> Result<Option<String>> {
        let p = self.root.join(format!("patterns/{name}.yaml"));
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(p)?))
    }

    /// Rollback by re-applying the archived content as a NEW revision. No
    /// history rewriting. Matches §4.2 "rollback produces a rollback commit"
    /// rule.
    pub fn rollback_pattern(&mut self, name: &str, to_version: u32) -> Result<PatternRevision> {
        let archive_path = self
            .root
            .join(format!("archive/patterns/{name}/v{to_version}.yaml"));
        if !archive_path.exists() {
            return Err(anyhow!(
                "no archived v{to_version} for pattern {name} at {}",
                archive_path.display()
            ));
        }
        let content = std::fs::read_to_string(&archive_path)?;
        self.save_pattern(name, &content, &format!("rollback to v{to_version}"))
    }

    /// Walk git log filtering commits that touch `patterns/<name>.yaml`.
    /// Returns chronological order (oldest first) for ease of reading.
    pub fn history(&self, name: &str) -> Result<Vec<HistoryEntry>> {
        let target = format!("patterns/{name}.yaml");
        let target_path = Path::new(&target);

        let mut walker = self.knowledge_repo.revwalk()?;
        walker.push_head()?;

        let mut entries = Vec::new();
        let mut version_counter: u32 = 0;

        // Collect in reverse-chrono first, then reverse + assign versions
        let mut tmp: Vec<HistoryEntry> = Vec::new();
        for oid in walker {
            let oid = oid?;
            let commit = self.knowledge_repo.find_commit(oid)?;
            if commit_touches_path(&self.knowledge_repo, &commit, target_path)? {
                tmp.push(HistoryEntry {
                    version: 0, // filled below
                    sha: short_sha(&oid),
                    message: commit
                        .message()
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string(),
                    timestamp: commit.time().seconds(),
                });
            }
        }
        // Reverse to chrono order; assign sequential versions
        tmp.reverse();
        for mut e in tmp {
            version_counter += 1;
            e.version = version_counter;
            entries.push(e);
        }
        Ok(entries)
    }

    /// True if last-recorded HEAD in `.mur-versions.yaml` differs from actual
    /// repo HEAD. False if no record exists (first run).
    pub fn detect_external_change(&self) -> Result<bool> {
        let index = self.root.join(".mur-versions.yaml");
        if !index.exists() {
            return Ok(false);
        }
        let recorded = std::fs::read_to_string(&index)?;
        let head = self.head_sha(&self.knowledge_repo)?;
        Ok(!recorded.contains(&head))
    }

    /// Refresh `.mur-versions.yaml` to current HEAD of both repos.
    pub fn rebuild_index(&self) -> Result<()> {
        let k = self.head_sha(&self.knowledge_repo)?;
        let a = self.head_sha(&self.agents_repo)?;
        let yaml = format!("knowledge_head: {k}\nagents_head: {a}\n");
        std::fs::write(self.root.join(".mur-versions.yaml"), yaml)?;
        Ok(())
    }

    /// Write `agents/<name>/profile.yaml` and commit to the agents repo.
    /// Mirror of [`Self::save_pattern`] for the execution-layer (R8 + future).
    pub fn save_agent_profile(
        &mut self,
        name: &str,
        profile_yaml: &str,
        reason: &str,
    ) -> Result<String> {
        let agent_dir_rel = PathBuf::from(name);
        let profile_rel = agent_dir_rel.join("profile.yaml");
        let profile_abs = self.root.join("agents").join(&profile_rel);
        std::fs::create_dir_all(profile_abs.parent().unwrap())?;
        std::fs::write(&profile_abs, profile_yaml)?;

        let sig = signature()?;
        let mut index = self.agents_repo.index()?;
        index.add_path(&profile_rel)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = self.agents_repo.find_tree(tree_id)?;
        let parent = self.agents_repo.head()?.peel_to_commit()?;
        let oid = self.agents_repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("agent({name}): {reason}"),
            &tree,
            &[&parent],
        )?;
        Ok(short_sha(&oid))
    }

    pub fn read_agent_profile(&self, name: &str) -> Result<Option<String>> {
        let p = self.root.join("agents").join(name).join("profile.yaml");
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(p)?))
    }

    /// Current HEAD SHA of the knowledge repo. Empty for unborn branch.
    pub fn knowledge_head(&self) -> Result<String> {
        self.head_sha(&self.knowledge_repo)
    }

    /// Current HEAD SHA of the agents repo. Empty for unborn branch.
    pub fn agents_head(&self) -> Result<String> {
        self.head_sha(&self.agents_repo)
    }

    fn current_pattern_version(&self, name: &str) -> Result<u32> {
        Ok(self.history(name)?.len() as u32)
    }

    fn head_sha(&self, repo: &Repository) -> Result<String> {
        match repo.head() {
            Ok(r) => Ok(r.peel_to_commit()?.id().to_string()),
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }
}

// ──────────── internals ────────────────────────────────────────────────────

fn open_or_init_repo(dir: &Path, gitignore: &str, init_msg: &str) -> Result<Repository> {
    if dir.join(".git").exists() {
        return Ok(Repository::open(dir)?);
    }
    let repo = Repository::init(dir)?;
    let gi_path = dir.join(".gitignore");
    std::fs::write(&gi_path, gitignore)?;

    let sig = signature()?;
    let mut index = repo.index()?;
    index.add_path(Path::new(".gitignore"))?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    repo.commit(Some("HEAD"), &sig, &sig, init_msg, &tree, &[])?;
    drop(tree);
    drop(index);
    Ok(repo)
}

fn commit_paths(repo: &Repository, paths: &[PathBuf], message: &str) -> Result<String> {
    let sig = signature()?;
    let mut index = repo.index()?;
    for p in paths {
        index.add_path(p)?;
    }
    // NOTE: do NOT add_all(".") here. R2 (Day 2 CI) found that add_all
    // walks the full working tree on every commit, turning save_pattern
    // into O(N) per call → O(N²) over the lifetime of the repo. At 3000
    // commits on Windows runner this caused 20+ min seed → CI timeout.
    // Production VersionedYamlStore MUST stage only explicit paths.
    // See plans/2026-05-18-continual-learning-versioned-evolution.md
    // §4.2 (to be patched with this finding on day 3).
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let parent = repo.head()?.peel_to_commit()?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;
    Ok(short_sha(&oid))
}

fn commit_touches_path(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    target: &Path,
) -> Result<bool> {
    let tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };
    let diff =
        repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
    let mut touched = false;
    diff.foreach(
        &mut |delta, _| {
            if delta.new_file().path() == Some(target)
                || delta.old_file().path() == Some(target)
            {
                touched = true;
            }
            true
        },
        None,
        None,
        None,
    )?;
    Ok(touched)
}

fn signature() -> Result<Signature<'static>> {
    Ok(Signature::now("mur-spike", "spike@mur.run")?)
}

fn short_sha(oid: &git2::Oid) -> String {
    let s = oid.to_string();
    s.chars().take(12).collect()
}

fn count_agent_dirs(agents_dir: &Path) -> usize {
    std::fs::read_dir(agents_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path().is_dir()
                        && !e.file_name().to_string_lossy().starts_with('.')
                })
                .count()
        })
        .unwrap_or(0)
}
