//! `mur fleet merge-concurrent <name>` — Model A post-hoc N-way line merge.
//! Default OFF; requires MUR_PARALLEL_CONCURRENT=1.
//! Disjoint hunks auto-merge; any overlap refuses --promote and reports for escalation.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::parallel::concurrent::{ConcurrentMerger, structural::StructuralMerger};
use crate::parallel::concurrent::stats::{OverlapStats, count_groups};
use crate::parallel::track::TrackSet;

const FLAG_ENV: &str = "MUR_PARALLEL_CONCURRENT";
const PARALLEL_BASE_FILE: &str = ".parallel-base";

fn flag_enabled() -> bool {
    std::env::var(FLAG_ENV).as_deref() == Ok("1")
}

/// Run `git show <rev>:<relpath>` in `cwd`; returns `None` if path didn't exist at that rev.
fn git_show(cwd: &Path, rev: &str, relpath: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("git")
        .arg("show")
        .arg(format!("{rev}:{relpath}"))
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() { Some(out.stdout) } else { None }
}

pub fn cmd_fleet_merge_concurrent(
    mur_home: &Path,
    fleet_name: &str,
    write_stats: bool,
    promote: bool,
    target: Option<&Path>,
) -> Result<()> {
    if !flag_enabled() {
        anyhow::bail!("set MUR_PARALLEL_CONCURRENT=1 to enable (experimental feature)");
    }

    super::store::load_fleet(mur_home, fleet_name)?;
    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` first to create track worktrees")?;

    if tracks.tracks.is_empty() {
        anyhow::bail!("fleet has no tracks");
    }

    let t0 = &tracks.tracks[0];
    let base_rev = std::fs::read_to_string(t0.worktree_path.join(PARALLEL_BASE_FILE))
        .context("read .parallel-base sentinel — was this run created by `mur fleet run`?")?
        .trim()
        .to_string();

    // Union of .rs files changed in any track vs base.
    let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for t in &tracks.tracks {
        let out = std::process::Command::new("git")
            .args(["diff", "--name-only", &base_rev, "HEAD"])
            .current_dir(&t.worktree_path)
            .output()
            .context("git diff --name-only")?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.ends_with(".rs") {
                changed.insert(line.to_string());
            }
        }
    }

    if changed.is_empty() {
        println!("No changed .rs files to merge.");
        return Ok(());
    }

    let merger = StructuralMerger;
    let result_dir = fleet_dir.join("cherry-result");
    let mut stat = OverlapStats { n_tracks: tracks.tracks.len(), ..Default::default() };
    let mut any_overlap = false;
    let mut written: Vec<String> = Vec::new();

    for rel in &changed {
        let base = git_show(&t0.worktree_path, &base_rev, rel).unwrap_or_default();
        let versions: Vec<(String, Vec<u8>)> = tracks.tracks.iter()
            .map(|t| {
                let bytes = std::fs::read(t.worktree_path.join(rel))
                    .unwrap_or_else(|_| base.clone());
                (t.config.name.clone(), bytes)
            })
            .collect();

        let outcome = merger.merge(&base, &versions)?;

        if write_stats {
            let (n_clean, n_overlap) = count_groups(&merger, &base, &versions)?;
            stat.files_compared += 1;
            stat.clean_groups += n_clean;
            stat.overlap_regions += n_overlap;
        }

        if !outcome.is_clean() {
            any_overlap = true;
            println!("OVERLAP: {rel} — {} region(s) need escalation", outcome.overlaps.len());
            for o in &outcome.overlaps {
                println!(
                    "  lines {}–{}: actors {:?}",
                    o.base_line_range.start, o.base_line_range.end, o.actor_ids
                );
            }
        }

        let dest_path = result_dir.join(rel);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest_path, &outcome.merged)?;
        written.push(rel.clone());
    }

    if write_stats {
        stat.finalize();
        let stats_path = fleet_dir.join("concurrent_stats.json");
        let json = serde_json::to_string_pretty(&stat)?;
        let tmp = stats_path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &stats_path)?;
        println!(
            "Spike-1: {}/{} hunk groups overlapped (rate={:.1}%)",
            stat.overlap_regions,
            stat.clean_groups + stat.overlap_regions,
            stat.overlap_rate * 100.0
        );
        println!("Stats written to {}", stats_path.display());
    }

    if promote {
        if any_overlap {
            anyhow::bail!("--promote refused: unresolved overlaps — resolve via judge/cherry first");
        }
        let dest: PathBuf = match target {
            Some(p) => p.to_path_buf(),
            None => super::cherry_cmd::project_root_from_worktree(&t0.worktree_path)
                .context("cannot find project root — pass --target <path>")?,
        };
        super::cherry_cmd::promote_cherry_result(&result_dir, &dest)?;
        let status = std::process::Command::new("cargo")
            .arg("check")
            .current_dir(&dest)
            .status();
        match status {
            Ok(s) if s.success() => println!("cargo check: OK"),
            Ok(_) => {
                for rel in &written {
                    let _ = std::process::Command::new("git")
                        .args(["checkout", "--", rel])
                        .current_dir(&dest)
                        .status();
                }
                anyhow::bail!("cargo check failed — reverted promoted files");
            }
            Err(e) => eprintln!("cargo check could not run ({e}) — leaving files in place"),
        }
    } else {
        println!("Run with --promote to copy into project (refused if overlaps remain).");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_gates_the_command() {
        unsafe { std::env::remove_var(FLAG_ENV) };
        assert!(!flag_enabled());
        unsafe { std::env::set_var(FLAG_ENV, "1") };
        assert!(flag_enabled());
        unsafe { std::env::remove_var(FLAG_ENV) };
    }
}
