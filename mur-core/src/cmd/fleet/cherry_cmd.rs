//! `mur fleet cherry <name>` — cherry-pick best functions across tracks into assembled files.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::store::load_fleet;
use crate::parallel::{
    cherry::{
        assemble::{TrackSource, assemble_file},
        conflict::check_conflicts,
        picker::cherry_pick,
    },
    judge::TrackScore,
    semantic::{SupportedLanguage, extract_units},
    state::ParallelStateDb,
    track::TrackSet,
};

pub fn cmd_fleet_cherry(
    mur_home: &Path,
    fleet_name: &str,
    auto: bool,
    promote: bool,
    target: Option<&Path>,
) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` then `mur fleet judge` first")?;
    let state_db = ParallelStateDb::open(&fleet_dir.join("parallel_state"))?;
    let rubric_ver = parallel.judge.rubric.version();

    if tracks.tracks.is_empty() {
        println!("No tracks found.");
        return Ok(());
    }

    // Collect unit scores per track by re-parsing worktrees.
    // unit_name → Vec<TrackScore>
    let mut scores_per_unit: std::collections::HashMap<String, Vec<TrackScore>> =
        std::collections::HashMap::new();

    // Pass 1: collect (track_name, content_hash) per unit.name across all
    // tracks. The competitor set for a unit is only known once every
    // track's implementation has been collected (issue #545: CyclicJudge
    // scores are relative to the competitor set).
    let mut by_name: std::collections::BTreeMap<String, Vec<(String, [u8; 32])>> =
        std::collections::BTreeMap::new();

    for t in &tracks.tracks {
        for entry in walkdir::WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let Ok(source) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(units) = extract_units(&source, SupportedLanguage::Rust) else {
                continue;
            };
            for unit in units {
                by_name
                    .entry(unit.name)
                    .or_default()
                    .push((t.config.name.clone(), unit.content_hash));
            }
        }
    }

    // Pass 2: now that the full competitor set per unit is known, compute
    // its set hash and look up each track's score against that exact set.
    for (name, members) in &by_name {
        let set_hash =
            ParallelStateDb::competitor_set_hash(&members.iter().map(|m| m.1).collect::<Vec<_>>());
        for (track, hash) in members {
            let Some(js) = state_db
                .get_score(hash, &set_hash, &rubric_ver)
                .ok()
                .flatten()
            else {
                continue;
            };
            scores_per_unit
                .entry(name.clone())
                .or_default()
                .push(TrackScore {
                    track_name: track.clone(),
                    score: js.score,
                    reasoning: js.reasoning,
                    low_confidence: false,
                });
        }
    }

    if scores_per_unit.is_empty() {
        println!("No scores found. Run `mur fleet judge {fleet_name}` first.");
        return Ok(());
    }

    let scores_slice: Vec<(&str, Vec<TrackScore>)> = scores_per_unit
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let plan = cherry_pick(&scores_slice);

    println!(
        "Cherry-picking {} units from {} tracks...",
        plan.selections.len(),
        tracks.tracks.len()
    );

    // Assemble output per file using first track as base.
    let base_track = &tracks.tracks[0];
    let result_dir = cherry_result_dir(mur_home, fleet_name);
    std::fs::create_dir_all(&result_dir)?;

    let mut written = 0usize;
    for entry in walkdir::WalkDir::new(&base_track.worktree_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
    {
        let file_path = entry.path();
        let Ok(base_source) = std::fs::read(file_path) else {
            continue;
        };

        // Collect track sources for this relative file path.
        let rel = file_path.strip_prefix(&base_track.worktree_path).ok();
        let mut track_sources: Vec<(String, Vec<u8>)> = Vec::new();
        for t in &tracks.tracks {
            let candidate = if let Some(rel) = rel {
                t.worktree_path.join(rel)
            } else {
                continue;
            };
            if let Ok(src) = std::fs::read(&candidate) {
                track_sources.push((t.config.name.clone(), src));
            }
        }
        let ts_refs: Vec<TrackSource<'_>> = track_sources
            .iter()
            .map(|(n, s)| TrackSource {
                track_name: n.as_str(),
                source: s.as_slice(),
            })
            .collect();

        // Check conflicts (P1: always empty, cargo check is the real gate).
        let conflicts = check_conflicts(&plan, &base_source, SupportedLanguage::Rust)?;
        if !conflicts.is_empty() && !auto {
            println!(
                "Dependency conflicts in {} — skipping (use --auto to override)",
                file_path.display()
            );
            continue;
        }

        let assembled = assemble_file(&base_source, &plan, &ts_refs, SupportedLanguage::Rust)?;

        // Write assembled file preserving directory structure.
        let out_path = if let Some(rel) = rel {
            result_dir.join(rel)
        } else {
            continue;
        };
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, assembled)?;
        written += 1;
    }

    if written == 0 {
        println!("No files assembled. Check that track worktrees contain .rs files.");
        return Ok(());
    }

    // Validate assembled result with cargo check.
    println!("Assembled {written} files → {}", result_dir.display());
    let check = std::process::Command::new("cargo")
        .args(["check", "--quiet", "--manifest-path"])
        .arg(result_dir.join("Cargo.toml"))
        .env("ORT_STRATEGY", "download")
        .output();
    match check {
        Ok(out) if out.status.success() => println!("cargo check: OK"),
        Ok(out) => eprintln!(
            "cargo check failed (non-fatal):\n{}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => eprintln!("cargo check could not run ({e}) — skipping validation"),
    }

    if promote {
        let dest = match target {
            Some(p) => p.to_path_buf(),
            None => project_root_from_worktree(&base_track.worktree_path)
                .context("cannot determine project root — pass --target <path>")?,
        };
        promote_cherry_result(&result_dir, &dest)?;
    } else {
        println!(
            "Run `mur fleet cherry {fleet_name} --promote` to copy the result into the project."
        );
    }
    Ok(())
}

/// Copy `result_dir` contents into `dest`, refusing if `dest` has uncommitted changes.
pub(super) fn promote_cherry_result(result_dir: &Path, dest: &Path) -> Result<()> {
    // Guard: refuse if destination has uncommitted changes.
    let status_out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dest)
        .output();
    match status_out {
        Ok(out) if !out.stdout.is_empty() => {
            anyhow::bail!(
                "destination {} has uncommitted changes — commit or stash first",
                dest.display()
            );
        }
        Err(e) => eprintln!("warn: git status check failed ({e}); proceeding anyway"),
        _ => {}
    }

    let mut copied = 0usize;
    for entry in walkdir::WalkDir::new(result_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(result_dir)?;
        let dst = dest.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(entry.path(), &dst)?;
        copied += 1;
    }
    println!("Promoted {copied} files → {}", dest.display());
    Ok(())
}

/// Resolve the main project root from a worktree path via git-common-dir.
pub(super) fn project_root_from_worktree(worktree: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common_dir_str = String::from_utf8(out.stdout).ok()?;
    let common_dir = PathBuf::from(common_dir_str.trim());
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        worktree.join(common_dir)
    };
    // common_dir is <project>/.git — parent is the project root
    common_dir.parent().map(|p| p.to_path_buf())
}

fn cherry_result_dir(mur_home: &Path, fleet_name: &str) -> PathBuf {
    mur_home
        .join("fleets")
        .join(fleet_name)
        .join("cherry-result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cherry_result_dir_name() {
        let mur_home = PathBuf::from("/home/user/.mur");
        let fleet_name = "my-fleet";
        let expected = mur_home
            .join("fleets")
            .join(fleet_name)
            .join("cherry-result");
        assert_eq!(cherry_result_dir(&mur_home, fleet_name), expected);
    }
}
