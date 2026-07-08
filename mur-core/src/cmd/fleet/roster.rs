//! `mur fleet add` / `mur fleet remove` — mutate membership in BOTH the fleet
//! manifest and the shared channel `fleet-<name>` so they never drift.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::ParticipantRole;

use super::store;

/// Split CLI-supplied member tokens on commas, trim whitespace, and drop
/// empties. The CLI help advertises "comma-separated member agent names"
/// (`mur-core/src/cli/actions.rs`) but clap alone doesn't split on `,` — this
/// makes that promise real. Accepts either `--members a,b c` or repeated
/// `--members a --members b`.
pub(crate) fn parse_member_args(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|s| s.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// True iff `agent` exists on disk under `<mur_home>/agents/<agent>/profile.yaml`,
/// matching the existence idiom used by `a2a_dial::canonicalize_agent_name`.
fn agent_exists(mur_home: &Path, agent: &str) -> bool {
    mur_home
        .join("agents")
        .join(agent)
        .join("profile.yaml")
        .is_file()
}

/// Add one or more agents as Delegate members. Idempotent per agent.
///
/// `agents` may contain comma-separated tokens (`"a,b"`) as well as
/// individually-passed names; both are flattened via [`parse_member_args`].
/// Every resolved name must exist as a real agent — unknown names are
/// rejected up front, before any membership mutation, so a batch containing
/// one bad name never partially applies.
pub fn cmd_fleet_add(mur_home: &Path, name: &str, agents: Vec<String>) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;

    let canonical: Vec<String> = parse_member_args(&agents)
        .into_iter()
        .map(|raw| crate::a2a_dial::canonicalize_agent_name(mur_home, &raw))
        .collect();

    let unknown: Vec<&str> = canonical
        .iter()
        .filter(|a| !agent_exists(mur_home, a))
        .map(|a| a.as_str())
        .collect();
    if !unknown.is_empty() {
        bail!(
            "unknown agent(s), not found in '{}': {}",
            mur_home.join("agents").display(),
            unknown.join(", ")
        );
    }

    let svc = mur_channel::ChannelService::open(mur_home)?;
    // ponytail: not transactional across N agents; channel ops are idempotent so re-running reconciles
    for agent in canonical {
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
    let agents = parse_member_args(&agents);

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

    /// Create a minimal on-disk agent profile so `agent_exists`/
    /// `canonicalize_agent_name` see it as a real agent.
    fn touch_agent(home: &Path, name: &str) {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), format!("name: {name}\n")).unwrap();
    }

    #[test]
    fn parse_member_args_splits_commas_and_trims_whitespace() {
        let names = parse_member_args(&["a,b".to_string(), "c".to_string()]);
        assert_eq!(names, vec!["a", "b", "c"]);

        let names = parse_member_args(&[" a , b ".to_string()]);
        assert_eq!(names, vec!["a", "b"]);

        // empties dropped
        let names = parse_member_args(&["a,,b".to_string(), "".to_string()]);
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn add_then_remove_member_syncs_fleet_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        touch_agent(home, "pm");
        touch_agent(home, "qa");
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
    fn add_splits_comma_separated_members() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        touch_agent(home, "a");
        touch_agent(home, "b");
        touch_agent(home, "c");
        create::cmd_fleet_create(home, "dev", vec![], None, Some("g".into()), None).unwrap();

        cmd_fleet_add(home, "dev", vec!["a,b".into(), "c".into()]).unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        assert!(f.members.contains(&"a".to_string()));
        assert!(f.members.contains(&"b".to_string()));
        assert!(f.members.contains(&"c".to_string()));
    }

    #[test]
    fn add_rejects_unknown_agent_without_partial_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        touch_agent(home, "pm");
        create::cmd_fleet_create(home, "dev", vec![], None, Some("g".into()), None).unwrap();

        // "pm" is real, "ghost" is not — batch must be rejected wholesale,
        // before either name is added to the channel/manifest.
        let result = cmd_fleet_add(home, "dev", vec!["pm,ghost".into()]);
        assert!(result.is_err(), "should reject batch with an unknown agent");

        let f = store::load_fleet(home, "dev").unwrap();
        assert!(
            !f.members.contains(&"pm".to_string()),
            "pm should not have been added; upfront validation must prevent partial mutation"
        );
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
