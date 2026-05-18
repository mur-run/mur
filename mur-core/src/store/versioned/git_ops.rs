//! git2 helpers for the versioned store.
// Not yet wired into the CLI — remove when integrated (E1-W3).
#![allow(dead_code)]
//!
//! Key constraint from FIN-1 (spike R2): `commit_paths` MUST stage only
//! the explicit paths the caller provides. Never call `index.add_all` on
//! the save hot path — at 3000 patterns that caused O(N²) filesystem ops
//! and CI timeout.

use anyhow::{Context, Result};
use git2::{Repository, Signature};
use std::path::{Path, PathBuf};

/// Open an existing repo at `dir`, or initialise one with a `.gitignore`
/// and an initial commit if `.git` is absent.
pub fn open_or_init_repo(dir: &Path, gitignore: &str, init_msg: &str) -> Result<Repository> {
    if dir.join(".git").exists() {
        return Repository::open(dir)
            .with_context(|| format!("open repo at {}", dir.display()));
    }

    let repo = Repository::init(dir)
        .with_context(|| format!("init repo at {}", dir.display()))?;
    std::fs::write(dir.join(".gitignore"), gitignore)?;

    let sig = signature()?;
    {
        let mut index = repo.index()?;
        index.add_path(Path::new(".gitignore"))?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        repo.commit(Some("HEAD"), &sig, &sig, init_msg, &tree, &[])?;
    }
    Ok(repo)
}

/// Stage exactly `paths` and create a commit. Returns 12-char short SHA.
///
/// FIN-1 compliance: explicit paths only — no `index.add_all`. Callers
/// must pass every path they changed, including archive copies.
pub fn commit_paths(repo: &Repository, paths: &[PathBuf], message: &str) -> Result<String> {
    let sig = signature()?;
    let mut index = repo.index()?;
    for p in paths {
        index
            .add_path(p)
            .with_context(|| format!("stage {}", p.display()))?;
    }
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let parent = repo.head()?.peel_to_commit()?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;
    Ok(short_sha(&oid))
}

/// Current HEAD commit SHA (full 40 chars). Empty string for unborn branch.
pub fn head_sha(repo: &Repository) -> Result<String> {
    match repo.head() {
        Ok(r) => Ok(r.peel_to_commit()?.id().to_string()),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

pub fn signature() -> Result<Signature<'static>> {
    Ok(Signature::now("mur", "store@mur.run")?)
}

pub fn short_sha(oid: &git2::Oid) -> String {
    oid.to_string().chars().take(12).collect()
}
