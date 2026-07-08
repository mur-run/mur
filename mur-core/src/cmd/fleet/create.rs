//! `mur fleet create` — write fleet.yaml + create the shared channel.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::fleet::{CONCIERGE_AGENT, Fleet, valid_fleet_name};
use mur_common::parallel::ParallelConfig;

use super::store;

pub fn cmd_fleet_create(
    mur_home: &Path,
    name: &str,
    members: Vec<String>,
    router: Option<String>,
    goal: Option<String>,
    parallel: Option<ParallelConfig>,
) -> Result<()> {
    if !valid_fleet_name(name) {
        bail!("invalid fleet name '{name}': use lowercase letters, digits, '-' or '_'");
    }
    if store::fleet_path(mur_home, name).exists() {
        bail!("fleet '{name}' already exists");
    }

    // Comma-split (mirrors `mur fleet add`'s handling of "--members a,b c")
    // then canonicalize member names and router to match the agent
    // runtime's on-disk ids. `create` does not validate agent existence
    // today; this change only adds splitting, not a new failure mode.
    let members: Vec<String> = super::roster::parse_member_args(&members)
        .into_iter()
        .map(|m| crate::a2a_dial::canonicalize_agent_name(mur_home, &m))
        .collect();
    let canonical_router: Option<String> =
        router.map(|r| crate::a2a_dial::canonicalize_agent_name(mur_home, &r));
    let router_name = canonical_router
        .clone()
        .unwrap_or_else(|| CONCIERGE_AGENT.to_string());

    let svc = mur_channel::ChannelService::open(mur_home)?;
    let ch = svc.create_for_fleet(name, &router_name, &members)?;

    let fleet = Fleet {
        name: name.to_string(),
        display_name: String::new(),
        goal: goal.unwrap_or_default(),
        router: canonical_router,
        team_id: None,
        members,
        channel_id: ch.id.clone(),
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        parallel,
    };
    store::save_fleet(mur_home, &fleet)?;
    println!("Created fleet '{name}' (channel {})", ch.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_writes_fleet_and_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("ship".into()),
            None,
        )
        .unwrap();
        let f = super::super::store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.channel_id, "fleet-dev");
        assert_eq!(f.goal, "ship");
        assert_eq!(f.router_or_concierge(), mur_common::fleet::CONCIERGE_AGENT);
        // second create errors (already exists)
        assert!(cmd_fleet_create(home, "dev", vec![], None, None, None).is_err());
    }
}
