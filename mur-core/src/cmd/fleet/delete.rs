//! `mur fleet delete <name> [--yes]` — delete fleet dir + shared channel.
//!
//! Member agents are NEVER touched (they are independent shared resources).
//! The channel audit history is removed along with the channel itself.

use std::io::Write as _;
use std::path::Path;

use anyhow::Result;

use super::store;

/// Delete a fleet: removes `~/.mur/fleets/<name>/` and the shared channel
/// `fleet-<name>`. Member agents are left intact.
///
/// Prompts for y/N confirmation unless `yes` is true.
pub fn cmd_fleet_delete(mur_home: &Path, name: &str, yes: bool) -> Result<()> {
    // Validate + ensure the fleet exists before asking for confirmation.
    let fleet = store::load_fleet(mur_home, name)?;

    if !yes && !confirm(name)? {
        println!("Aborted — fleet '{name}' left intact.");
        return Ok(());
    }

    // Remove channel first (audit history goes with it).
    let svc = mur_channel::ChannelService::open(mur_home)?;
    svc.delete_channel(&fleet.channel_id)?;

    // Remove the fleet directory tree (manifest, jobs/, sentinels).
    let dir = store::fleet_dir(mur_home, name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }

    println!("Fleet '{name}' deleted (channel '{}' removed; member agents left intact).", fleet.channel_id);
    Ok(())
}

/// Interactive y/N confirmation (skipped when `--yes`).
/// Destructive: also removes the channel's audit history.
fn confirm(name: &str) -> Result<bool> {
    print!("Delete fleet '{name}' and its channel history? Members NOT deleted. [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{create, store};

    #[test]
    fn delete_removes_fleet_dir_and_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("g".into())).unwrap();
        assert!(store::fleet_path(home, "dev").exists());
        let svc = mur_channel::ChannelService::open(home).unwrap();
        assert!(svc.store().load_manifest("fleet-dev").is_ok());

        cmd_fleet_delete(home, "dev", true).unwrap();

        assert!(!store::fleet_dir(home, "dev").exists(), "fleet dir must be gone");
        let svc = mur_channel::ChannelService::open(home).unwrap();
        assert!(
            svc.store().load_manifest("fleet-dev").is_err(),
            "channel must be gone"
        );
    }

    #[test]
    fn delete_unknown_fleet_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(cmd_fleet_delete(tmp.path(), "ghost", true).is_err());
    }
}
