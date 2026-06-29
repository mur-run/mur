#![allow(dead_code, unused_imports)]
//! `mur fleet partition-plan` and `mur fleet merge` commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::parallel::partition::{
    PartitionPlan, RegionAssignment,
    merge::merge_partition_file,
    planner::{plan_partition, validate_coverage},
};
use crate::parallel::semantic::{SupportedLanguage, extract_units};
use crate::parallel::track::TrackSet;

use super::store::{fleet_dir, load_fleet};

/// Resolve the repo root from the current working directory via git.
fn cwd_git_root() -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("spawn git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!("not inside a git repo — run from the project root");
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// `mur fleet partition-plan <name>` — preview how the file is split across tracks.
pub fn cmd_fleet_partition_plan(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;
    let partition = parallel
        .partition
        .as_ref()
        .context("fleet parallel.mode is not 'partition' or partition.target_file is missing")?;

    let project_root = cwd_git_root()?;
    let file_path = project_root.join(&partition.target_file);
    let source =
        std::fs::read(&file_path).with_context(|| format!("read {}", file_path.display()))?;

    let units = extract_units(&source, SupportedLanguage::Rust)
        .with_context(|| format!("parse {}", file_path.display()))?;
    let track_names: Vec<String> = parallel.tracks.iter().map(|t| t.name.clone()).collect();
    let plan = plan_partition(&units, &track_names)?;
    validate_coverage(&plan, &units)?;

    println!(
        "Partition plan for `{}` ({} tracks):",
        partition.target_file,
        track_names.len()
    );
    println!("{}", render_plan(&plan));
    Ok(())
}

/// `mur fleet merge <name> [--promote] [--target <path>]`
pub fn cmd_fleet_merge(
    mur_home: &Path,
    fleet_name: &str,
    promote: bool,
    target: Option<&Path>,
) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;
    let partition = parallel
        .partition
        .as_ref()
        .context("fleet parallel.mode is not 'partition' or partition.target_file is missing")?;

    let rel = PathBuf::from(&partition.target_file);
    let fleet_dir = fleet_dir(mur_home, fleet_name);
    let tracks =
        TrackSet::load(&fleet_dir).context("load track worktrees — run 'mur fleet run' first")?;

    // Use track-0's copy as base source.
    let base_track = tracks
        .tracks
        .first()
        .context("no tracks found in fleet worktrees")?;
    let base = std::fs::read(base_track.worktree_path.join(&rel))
        .with_context(|| format!("read base from track-0: {}", rel.display()))?;

    let units = extract_units(&base, SupportedLanguage::Rust)
        .with_context(|| format!("parse {}", rel.display()))?;
    let track_names: Vec<String> = parallel.tracks.iter().map(|t| t.name.clone()).collect();
    let plan = plan_partition(&units, &track_names)?;
    validate_coverage(&plan, &units)?;

    let mut agent_sources: Vec<(String, Vec<u8>)> = Vec::new();
    for t in &tracks.tracks {
        let p = t.worktree_path.join(&rel);
        let src = std::fs::read(&p)
            .with_context(|| format!("read {} for track {}", p.display(), t.config.name))?;
        agent_sources.push((t.config.name.clone(), src));
    }

    let merged = merge_partition_file(&base, &plan, &agent_sources, SupportedLanguage::Rust)?;

    // Write into fleet's cherry-result dir, preserving relative path.
    let result_dir = fleet_dir.join("cherry-result");
    let out_path = result_dir.join(&rel);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, &merged)?;
    println!("Merged {} → {}", rel.display(), out_path.display());

    if promote {
        let dest = match target {
            Some(p) => p.to_path_buf(),
            None => super::cherry_cmd::project_root_from_worktree(&tracks.tracks[0].worktree_path)
                .context("cannot determine project root — pass --target <path>")?,
        };
        super::cherry_cmd::promote_cherry_result(&result_dir, &dest)?;
    } else {
        println!("Run `mur fleet merge {fleet_name} --promote` to copy into project.");
    }
    Ok(())
}

/// Render a PartitionPlan as human-readable text.
pub(crate) fn render_plan(plan: &PartitionPlan) -> String {
    let mut s = String::new();
    for a in &plan.assignments {
        s.push_str(&format!(
            "{} ({} items):\n",
            a.track_name,
            a.unit_names.len()
        ));
        for name in &a.unit_names {
            s.push_str(&format!("  - {name}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plan_shows_tracks_and_items() {
        let plan = PartitionPlan {
            assignments: vec![
                RegionAssignment {
                    track_name: "t0".into(),
                    unit_names: vec!["a".into(), "b".into()],
                },
                RegionAssignment {
                    track_name: "t1".into(),
                    unit_names: vec!["c".into()],
                },
            ],
        };
        let out = render_plan(&plan);
        assert!(out.contains("t0 (2 items)"));
        assert!(out.contains("- a"));
        assert!(out.contains("t1 (1 items)"));
        assert!(out.contains("- c"));
    }

    #[test]
    fn merge_writes_target_into_result_dir() {
        let base = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 0 }\n";
        let a0 = b"fn alpha() -> i32 { 11 }\nfn beta() -> i32 { 0 }\n";
        let a1 = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 22 }\n";

        let plan = PartitionPlan {
            assignments: vec![
                RegionAssignment {
                    track_name: "track-0".into(),
                    unit_names: vec!["alpha".into()],
                },
                RegionAssignment {
                    track_name: "track-1".into(),
                    unit_names: vec!["beta".into()],
                },
            ],
        };
        let merged = merge_partition_file(
            base,
            &plan,
            &[
                ("track-0".to_string(), a0.to_vec()),
                ("track-1".to_string(), a1.to_vec()),
            ],
            SupportedLanguage::Rust,
        )
        .unwrap();
        let out = String::from_utf8(merged).unwrap();
        assert!(out.contains("11") && out.contains("22"));
    }
}
