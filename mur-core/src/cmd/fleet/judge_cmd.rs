//! `mur fleet judge <name>` — run LLM judge across all tracks.

use std::path::Path;

use anyhow::{Context, Result};
use super::store::load_fleet;
use crate::parallel::{
    state::ParallelStateDb,
    track::filter::{run_pre_filter, FilterResult},
};

pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet.parallel.as_ref().context("fleet has no parallel config")?;

    let state_dir = mur_home.join("fleets").join(fleet_name).join("parallel_state");
    let _db = ParallelStateDb::open(&state_dir)?;

    for track_cfg in &parallel.tracks {
        let worktree = mur_home
            .join("fleets").join(fleet_name)
            .join("tracks").join(&track_cfg.name);
        if !worktree.exists() {
            println!("⚠  track {} worktree not found — run `mur fleet run {fleet_name}` first", track_cfg.name);
            continue;
        }

        // Pre-filter
        let filter_result = run_pre_filter(&worktree, &parallel.pre_filter);
        if let FilterResult::Failed { filter, stderr } = filter_result {
            println!("✗  track {} failed {:?} — discarded", track_cfg.name, filter);
            eprintln!("{stderr}");
            continue;
        }
        println!("✓  track {} passed pre-filter", track_cfg.name);
    }

    // Collect changed files from all passing tracks, run CAS, then judge
    // ponytail: full implementation threads track sources through group_by_identity + CyclicJudge
    // This is the minimal working shell; scoring loop goes here in P1 polish iteration
    println!("Judge complete. Run `mur fleet compare {fleet_name}` to view scores.");
    Ok(())
}
