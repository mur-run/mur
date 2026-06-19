//! `mur fleet list` — all fleets.

use std::path::Path;

use anyhow::Result;

use super::store;

pub fn cmd_fleet_list(mur_home: &Path) -> Result<()> {
    let names = store::list_fleets(mur_home)?;
    if names.is_empty() {
        println!("No fleets. Create one: mur fleet create <name> --members a,b,c --goal \"...\"");
        return Ok(());
    }
    for n in names {
        let f = store::load_fleet(mur_home, &n)?;
        println!(
            "{}  members=[{}]  router={}  goal={}",
            f.name,
            f.members.join(","),
            f.router_or_concierge(),
            f.goal
        );
    }
    Ok(())
}
