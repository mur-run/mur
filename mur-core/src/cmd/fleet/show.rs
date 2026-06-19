//! `mur fleet show` — roster + goal.

use std::path::Path;

use anyhow::Result;

use super::store;

pub fn cmd_fleet_show(mur_home: &Path, name: &str) -> Result<()> {
    let f = store::load_fleet(mur_home, name)?;
    println!("Fleet: {}", f.name);
    println!("Goal: {}", f.goal);
    println!("Router: {}", f.router_or_concierge());
    println!("Members: {}", f.members.join(", "));
    println!("Channel: {}", f.channel_id);
    if super::control::is_stopped(mur_home, name) {
        println!("Status: STOPPED (kill-switch active — `mur fleet start {name}`)");
    }
    if !f.rules.is_empty() {
        println!("Rules: {}", f.rules.join(", "));
    }
    if !f.skills.is_empty() {
        println!("Skills: {}", f.skills.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_errors_when_missing_ok_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(cmd_fleet_show(home, "dev").is_err());
        super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None).unwrap();
        assert!(cmd_fleet_show(home, "dev").is_ok());
    }
}
