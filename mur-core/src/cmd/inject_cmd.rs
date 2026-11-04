use anyhow::Result;

use crate::inject;
use crate::retrieve::skill_candidates::load_skill_candidates;
use crate::store::workflow_yaml::WorkflowYamlStore;

pub(crate) async fn cmd_inject(query: &str) -> Result<()> {
    crate::auth::heartbeat();
    use crate::retrieve::gate::{Tier as GateTier, evaluate_query};
    use crate::retrieve::scoring::score_and_rank_generic;
    use mur_common::skill::stats::LifecycleState;

    let outcome = evaluate_query(query);
    if outcome.tier == GateTier::Skip {
        eprintln!(
            "# No skills matched (gate: skip, score={:.2}, reasons={:?})",
            outcome.score, outcome.reasons
        );
        return Ok(());
    }

    let mur_home = mur_common::home::mur_home();
    let skills_dir = mur_home.join("skills");

    let candidates = load_skill_candidates(&skills_dir, &mur_home)?;
    let active: Vec<_> = candidates
        .into_iter()
        .filter(|s| !matches!(s.stats.lifecycle_state, LifecycleState::Archived))
        .collect();

    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    let results = score_and_rank_generic(query, active);

    // Resolve git worktrees to the main repo name so injection-learning scopes
    // per-repo (matching the codebase index), not per-branch-worktree.
    let project_name = std::env::current_dir()
        .ok()
        .map(|p| crate::codebase::scanner::project_name_from_path(&p))
        .unwrap_or_default();
    let injected_items: Vec<_> = results
        .into_iter()
        .map(|scored| scored.item.to_injected_item())
        .collect();

    let output = inject::hook::format_unified_injection_items(&injected_items, &workflows, 2000);

    if output.is_empty() {
        eprintln!("# No relevant skills found");
    } else {
        inject::hook::record_injection_items(query, &project_name, &injected_items);
        inject::hook::record_cooccurrence_for_items(&injected_items);
        print!("{}", output);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::sync::inbox::Inbox;
    use mur_common::{
        Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
    };
    use uuid::Uuid;

    /// Legacy pattern-targeted signals (frozen wire v1) must still drain
    /// cleanly: parsed, acknowledged as skipped, file removed — never an
    /// error that would leave the inbox wedged.
    #[test]
    fn inbox_drain_skips_legacy_pattern_signals() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox_dir).unwrap();
        let signal = Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "bot".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "drain-test".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
            sig: None,
            key_version: 0,
        };
        let inbox = Inbox::new(&inbox_dir).unwrap();
        let path = inbox.receive(&signal).unwrap();
        let report = inbox.apply_all().unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!path.exists(), "skipped signal file must be removed");
    }
}
