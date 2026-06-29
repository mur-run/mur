//! `mur fleet compare <name>` — show per-unit scores across all tracks.

use std::path::Path;

use anyhow::{Context, Result};

use crate::parallel::state::ParallelStateDb;

use super::store::load_fleet;

pub fn cmd_fleet_compare(mur_home: &Path, fleet_name: &str, unit_filter: Option<&str>) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet.parallel.as_ref().context("fleet has no parallel config")?;
    let state_dir = mur_home.join("fleets").join(fleet_name).join("parallel_state");
    // Open state DB so it exists even if empty; real reads happen in Task 12.
    let _db = ParallelStateDb::open(&state_dir)?;
    let _rubric_ver = parallel.judge.rubric.version();
    let _ = unit_filter;

    // Load score for each track × unit from LMDB.
    // For now print a summary; rich table follows in P1 polish.
    println!("Fleet: {fleet_name}  Tracks: {}", parallel.tracks.len());
    println!(
        "{:<25} {}",
        "Unit",
        parallel
            .tracks
            .iter()
            .map(|t| format!("{:<12}", t.name))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("{}", "-".repeat(70));

    // Stub — full implementation requires TrackSet in state DB.
    println!("(Run `mur fleet judge {fleet_name}` first to populate scores)");
    Ok(())
}
