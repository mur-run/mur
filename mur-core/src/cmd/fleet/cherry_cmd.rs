//! `mur fleet cherry <name>` — execute cherry-pick assembly.

use std::path::Path;

use anyhow::Result;

pub fn cmd_fleet_cherry(mur_home: &Path, fleet_name: &str, auto: bool) -> Result<()> {
    let _ = (mur_home, fleet_name, auto);
    todo!("implemented in Task 12")
}
