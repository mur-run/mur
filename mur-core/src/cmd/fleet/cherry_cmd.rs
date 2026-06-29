//! `mur fleet cherry <name>` — execute cherry-pick assembly.

use std::path::Path;

use super::store::load_fleet;
use crate::parallel::state::ParallelStateDb;
use anyhow::{Context, Result};

pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, _auto: bool) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;
    let state_dir = mur_home
        .join("fleets")
        .join(fleet_name)
        .join("parallel_state");
    let _db = ParallelStateDb::open(&state_dir)?;

    println!(
        "Cherry-picking best functions from {} tracks...",
        parallel.tracks.len()
    );
    // ponytail: full cherry loop (load scores → cherry_pick → assemble_file → cargo check → write)
    // P1 alpha: print the plan; write output to fleets/<name>/cherry-result/
    println!("Use `mur fleet promote {fleet_name} cherry` to apply the result.");
    Ok(())
}
