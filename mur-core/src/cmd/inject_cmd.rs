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

    let home = dirs::home_dir().expect("no home dir");
    let mur_home = home.join(".mur");
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
    use crate::store::yaml::YamlStore;
    use crate::sync::inbox::Inbox;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content, Pattern, Tier};
    use mur_common::{
        Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
    };
    use uuid::Uuid;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: "test".into(),
                content: Content::Plain("body".into()),
                tier: Tier::Session,
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn inbox_drain_applies_signals_to_store() {
        let tmp = tempfile::tempdir().unwrap();
        let patterns_dir = tmp.path().join("patterns");
        let inbox_dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox_dir).unwrap();
        let store = YamlStore::new(patterns_dir).unwrap();
        store.save(&make_pattern("drain-test")).unwrap();
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
        inbox.receive(&signal).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1);
        let p = store.get("drain-test").unwrap();
        assert_eq!(p.evidence.success_signals, 1);
    }
}
