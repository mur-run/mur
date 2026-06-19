//! `mur fleet run` — one iteration: fan the goal out to each member over the
//! shared channel via the existing DAG executor (delegation), then print replies.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::channel::ChannelActor;
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use super::store;

/// Phase 1 "plan": one parallel delegate-step per member, each handed the goal.
/// (Phase 2 replaces this with a router-produced DAG.)
pub fn build_fleet_procedure(goal: &str, members: &[String]) -> Procedure {
    Procedure {
        variables: vec![],
        steps: members
            .iter()
            .map(|m| ProcedureStep {
                description: format!("{m}: {goal}"),
                intent: Some(goal.to_string()),
                delegate_to: Some(m.clone()),
                id: Some(m.clone()),
                ..Default::default()
            })
            .collect(),
    }
}

pub async fn cmd_fleet_run(mur_home: &Path, name: &str) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        bail!("fleet '{name}' has no members");
    }
    let proc = build_fleet_procedure(&fleet.goal, &fleet.members);
    let opts = crate::executor::dag::DagExecOptions {
        yes: true,
        channel_id: Some(fleet.channel_id.clone()),
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        ..Default::default()
    };
    // skill_name here is just a label for the run; the fleet channel id is reused.
    let out = crate::executor::dag::execute_dag(mur_home, &fleet.channel_id, &proc, &opts).await?;
    if let Some(t) = out.output_text.filter(|t| !t.is_empty()) {
        println!("{t}");
    }

    // Tail agent-authored replies written into the shared channel (peer-writes-own).
    // Note: prints payload["text"]; confirm the exact reply payload shape in the live Harness test.
    let svc = mur_channel::ChannelService::open(mur_home)?;
    for ev in svc.load_events(&fleet.channel_id)? {
        if let (ChannelActor::Agent { id }, Some(text)) = (
            &ev.actor,
            ev.payload
                .get("text")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty()),
        ) {
            println!("[{id}] {text}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fleet_procedure_one_delegate_step_per_member() {
        let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()]);
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
        assert_eq!(p.steps[0].intent.as_deref(), Some("ship it"));
        assert!(p.steps[0].depends_on.is_empty()); // parallel rank 0
    }
}
