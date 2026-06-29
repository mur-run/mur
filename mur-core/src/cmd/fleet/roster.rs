//! `mur fleet add` / `mur fleet remove` — mutate membership in BOTH the fleet
//! manifest and the shared channel `fleet-<name>` so they never drift.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::ParticipantRole;

use super::store;

/// Add one or more agents as Delegate members. Idempotent per agent.
pub fn cmd_fleet_add(mur_home: &Path, name: &str, agents: Vec<String>) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    let svc = mur_channel::ChannelService::open(mur_home)?;
    // ponytail: not transactional across N agents; channel ops are idempotent so re-running reconciles
    for raw in agents {
        let agent = crate::a2a_dial::canonicalize_agent_name(mur_home, &raw);
        if fleet.members.contains(&agent) {
            println!("'{agent}' is already a member of '{name}'.");
            continue;
        }
        svc.add_participant(&fleet.channel_id, &agent, ParticipantRole::Delegate)?;
        fleet.members.push(agent.clone());
        println!("Added '{agent}' to fleet '{name}'.");
    }
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}

/// Remove one or more agents. Refuses the current router; no-ops on non-members.
pub fn cmd_fleet_remove(mur_home: &Path, name: &str, agents: Vec<String>) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    let router = fleet.router_or_concierge().to_string();

    // Upfront validation: reject the entire batch if ANY agent is the router,
    // before touching the channel or manifest.
    for raw in &agents {
        let agent = crate::a2a_dial::canonicalize_agent_name(mur_home, raw);
        if agent == router {
            bail!("router '{agent}' cannot be removed from '{name}'; set a new router first");
        }
    }

    let svc = mur_channel::ChannelService::open(mur_home)?;
    // ponytail: not transactional across N agents; channel ops are idempotent so re-running reconciles
    for raw in agents {
        let agent = crate::a2a_dial::canonicalize_agent_name(mur_home, &raw);
        if !fleet.members.contains(&agent) {
            println!("'{agent}' is not a member of '{name}'.");
            continue;
        }
        svc.remove_participant(&fleet.channel_id, &agent)?;
        fleet.members.retain(|m| m != &agent);
        println!("Removed '{agent}' from fleet '{name}'.");
    }
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{create, store};
    use super::*;

    #[test]
    fn add_then_remove_member_syncs_fleet_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("g".into()), None)
            .unwrap();

        cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap();
        cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap(); // idempotent
        let f = store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.members.iter().filter(|m| *m == "qa").count(), 1);

        cmd_fleet_remove(home, "dev", vec!["qa".into()]).unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        assert!(!f.members.contains(&"qa".to_string()));
    }

    #[test]
    fn remove_router_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // router defaults to the concierge "mur"; make it a member too
        create::cmd_fleet_create(
            home,
            "dev",
            vec!["mur".into(), "pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();
        assert!(cmd_fleet_remove(home, "dev", vec!["mur".into()]).is_err());
    }

    #[test]
    fn remove_router_mixed_batch_no_partial_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // router defaults to "mur"; pm is a non-router member
        create::cmd_fleet_create(
            home,
            "dev",
            vec!["mur".into(), "pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        // Batch with router in a non-first position: ["pm", "mur"]
        // Without upfront validation, "pm" would be channel-removed before the router bail.
        let result = cmd_fleet_remove(home, "dev", vec!["pm".into(), "mur".into()]);
        assert!(result.is_err(), "should reject batch containing the router");

        // pm must still be a member — no partial mutation occurred
        let fleet = store::load_fleet(home, "dev").unwrap();
        assert!(
            fleet.members.contains(&"pm".to_string()),
            "pm should not have been removed; upfront validation must prevent partial mutation"
        );
    }
}
