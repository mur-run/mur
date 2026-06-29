//! `mur fleet judge <name> [--stats]` — run LLM judge across all tracks.

use anyhow::{Context, Result};
use std::path::Path;

use super::store::load_fleet;
use crate::parallel::{JudgeStats, run_judge_pipeline_async, state::ParallelStateDb, track::TrackSet};

pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str, write_stats: bool) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);

    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` first to create track worktrees")?;

    let state_db = ParallelStateDb::open(&fleet_dir.join("parallel_state"))?;

    let rt = tokio::runtime::Runtime::new()?;
    let stats = rt.block_on(run_judge_pipeline_async(&tracks, parallel, &state_db))?;

    if write_stats {
        write_judge_stats(&fleet_dir, &stats)?;
    }

    println!("Judge complete. Run `mur fleet compare {fleet_name}` to view scores.");
    Ok(())
}

fn write_judge_stats(fleet_dir: &Path, stats: &JudgeStats) -> Result<()> {
    let path = fleet_dir.join("judge_stats.json");
    let json = serde_json::to_string_pretty(stats)?;
    // atomic write: temp + rename
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    eprintln!("stats written to {}", path.display());
    Ok(())
}
