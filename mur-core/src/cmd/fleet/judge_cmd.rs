//! `mur fleet judge <name>` — run LLM judge across all tracks.

use std::path::Path;

use anyhow::Result;

pub fn cmd_fleet_judge(mur_home: &Path, fleet_name: &str) -> Result<()> {
    let _ = (mur_home, fleet_name);
    todo!("implemented in Task 12")
}
