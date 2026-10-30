//! `mur suggest` and draft-workflow creation shared with nudge-accept.

use super::*;

pub(crate) fn cmd_suggest(create: bool, accept: Option<&str>, dismiss: Option<&str>) -> Result<()> {
    // ─── Nudge accept/dismiss (early exit) ────────────────────────────
    if let Some(id) = accept {
        return cmd_suggest_accept(id);
    }
    if let Some(id) = dismiss {
        return cmd_suggest_dismiss(id);
    }

    // Drain companion nudge responses before listing pending.
    let _ = crate::nudge::companion::drain_nudge_responses_in(
        &crate::store::yaml::default_mur_dir(),
        &|c| create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids),
    );

    use evolve::cooccurrence::CooccurrenceMatrix;

    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // ─── Part 1: Workflow composition from co-occurrence ─────────────

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Knowledge ↔ Workflow Intelligence\n");
    println!("── Co-occurrence Data ──");
    println!("  Tracked pairs: {}", matrix.pair_count());

    // Part 1: Co-occurrence data shown above (composition suggestions removed with Pattern system)

    // ─── Part 2 (removed): Workflow decomposition into patterns ──────────────
    // Pattern decomposition removed — skills use skill/lifecycle.rs evolve path.
    let _ = (&workflows, create);

    // ─── Part 3: Pending nudges ───────────────────────────────────────

    let nudge_path = crate::nudge::NudgeLedger::default_path();
    if let Ok(ledger) = crate::nudge::NudgeLedger::load(&nudge_path) {
        let now = chrono::Utc::now();
        let mut pending: Vec<(&String, &crate::nudge::NudgeRecord, &str)> = Vec::new();
        for (id, rec) in &ledger.records {
            let status = match &rec.state {
                crate::nudge::NudgeState::Surfaced => "surfaced",
                crate::nudge::NudgeState::Snoozed { until } => {
                    let expired = chrono::DateTime::parse_from_rfc3339(until)
                        .map(|u| now >= u.with_timezone(&chrono::Utc))
                        .unwrap_or(true);
                    if expired {
                        "snoozed-expired"
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            pending.push((id, rec, status));
        }

        if !pending.is_empty() {
            println!("── Pending Workflow Nudges ──\n");
            for (id, rec, status) in &pending {
                let short_id = if id.len() > 8 { &id[..8] } else { id };
                let title = rec
                    .candidate
                    .as_ref()
                    .map(|c| c.title.as_str())
                    .unwrap_or("(unknown)");
                let sessions = rec.candidate.as_ref().map(|c| c.session_count).unwrap_or(0);
                println!(
                    "  [{}] {} (seen in {} sessions) [{}]",
                    short_id, title, sessions, status
                );
            }
            println!();
            println!("  Accept:  mur suggest --accept <id>");
            println!("  Dismiss: mur suggest --dismiss <id>");
            println!();
        }
    }

    // ─── Summary ─────────────────────────────────────────────────────

    if !create && !workflows.is_empty() {
        println!("Run `mur suggest --create` to auto-create suggested items as drafts.");
    }

    Ok(())
}

pub(crate) fn cmd_suggest_accept(id: &str) -> Result<()> {
    let config_path = crate::store::yaml::default_mur_dir().join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger,
        id,
        crate::nudge::NudgeDecision::Accept,
        cfg.nudge.snooze_days,
        chrono::Utc::now(),
        &|c| create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids),
    )?;
    ledger.save(&path)?;
    println!(
        "✓ Saved workflow draft from nudge {}. Run it with `mur run <name>`.",
        id
    );
    Ok(())
}

pub(crate) fn cmd_suggest_dismiss(id: &str) -> Result<()> {
    let config_path = crate::store::yaml::default_mur_dir().join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger,
        id,
        crate::nudge::NudgeDecision::Dismiss,
        cfg.nudge.snooze_days,
        chrono::Utc::now(),
        &|_| Ok(()),
    )?;
    ledger.save(&path)?;
    println!("✓ Dismissed nudge {}.", id);
    Ok(())
}

/// Build and save a draft Workflow. Shared by `mur suggest --create` and nudge-accept.
pub fn create_draft_workflow_in(
    store: &crate::store::workflow_yaml::WorkflowYamlStore,
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
) -> anyhow::Result<()> {
    if store.exists(name) {
        return Ok(()); // idempotent: don't clobber an existing workflow
    }
    let base = KnowledgeBase {
        name: name.to_string(),
        description: description.to_string(),
        content: Content::Plain(trigger.to_string()),
        maturity: mur_common::knowledge::Maturity::Draft,
        ..Default::default()
    };
    let wf = mur_common::workflow::Workflow {
        base,
        steps: vec![],
        variables: vec![],
        source_sessions: source_sessions.to_vec(),
        trigger: trigger.to_string(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };
    store.save(&wf)
}

/// Convenience over the default store.
pub fn create_draft_workflow(
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
) -> anyhow::Result<()> {
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    create_draft_workflow_in(&store, name, description, trigger, source_sessions)
}
