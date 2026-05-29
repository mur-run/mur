//! Integration tests for the nudge workflow: accept/dismiss + end-to-end.

use mur_core::nudge::{NudgeDecision, NudgeEmitter, NudgeLedger, NudgeState, WorkflowCandidate};

fn cand(id: &str, suggested_name: &str) -> WorkflowCandidate {
    WorkflowCandidate {
        id: id.into(),
        title: format!("Test behavior {}", id),
        suggested_name: suggested_name.into(),
        steps_preview: vec![],
        session_count: 3,
        evidence_session_ids: vec!["s1".into()],
    }
}

#[test]
fn accept_creates_draft_and_marks_ledger() {
    let home = tempfile::tempdir().unwrap();
    let nudge_path = home.path().join("nudges.json");

    // Seed a ledger with one surfaced candidate
    let mut ledger = NudgeLedger::default();
    NudgeEmitter::emit_pending(
        &mut ledger,
        &[cand("abc", "test-then-commit")],
        chrono::Utc::now(),
    );
    ledger.save(&nudge_path).unwrap();

    // Accept via the emitter with a creator that writes to temp store
    let wf_dir = home.path().join("workflows");
    let wf_store = mur_core::store::workflow_yaml::WorkflowYamlStore::new(wf_dir.clone()).unwrap();
    let mut ledger = NudgeLedger::load(&nudge_path).unwrap();
    NudgeEmitter::apply_decision(
        &mut ledger,
        "abc",
        NudgeDecision::Accept,
        7,
        chrono::Utc::now(),
        &|c| {
            mur_core::cmd::workflow::create_draft_workflow_in(
                &wf_store,
                &c.suggested_name,
                &c.title,
                "",
                &c.evidence_session_ids,
            )
        },
    )
    .unwrap();
    ledger.save(&nudge_path).unwrap();

    // Verify ledger state
    assert!(matches!(
        ledger.get("abc").unwrap().state,
        NudgeState::Accepted
    ));

    // Verify draft workflow created
    assert!(wf_store.exists("test-then-commit"));
}

#[test]
fn dismiss_marks_ledger_and_never_resurfaces() {
    let home = tempfile::tempdir().unwrap();
    let nudge_path = home.path().join("nudges.json");

    let mut ledger = NudgeLedger::default();
    NudgeEmitter::emit_pending(&mut ledger, &[cand("abc", "wf-name")], chrono::Utc::now());
    ledger.save(&nudge_path).unwrap();

    let mut ledger = NudgeLedger::load(&nudge_path).unwrap();
    NudgeEmitter::apply_decision(
        &mut ledger,
        "abc",
        NudgeDecision::Dismiss,
        7,
        chrono::Utc::now(),
        &|_| Ok(()),
    )
    .unwrap();
    ledger.save(&nudge_path).unwrap();

    assert!(matches!(
        ledger.get("abc").unwrap().state,
        NudgeState::Dismissed
    ));

    // Dismissed candidate is not actionable
    let actionable = ledger.filter_actionable(&[cand("abc", "wf-name")], chrono::Utc::now(), 10);
    assert!(actionable.is_empty());
}

#[test]
fn end_to_end_detect_accept_no_resurface() {
    let home = tempfile::tempdir().unwrap();
    let nudge_path = home.path().join("nudges.json");

    // 1. Emit a pending nudge for candidate "abc"
    let mut ledger = NudgeLedger::default();
    NudgeEmitter::emit_pending(
        &mut ledger,
        &[cand("abc", "my-workflow")],
        chrono::Utc::now(),
    );
    ledger.save(&nudge_path).unwrap();

    // 2. Accept it
    let wf_dir = home.path().join("workflows");
    let wf_store = mur_core::store::workflow_yaml::WorkflowYamlStore::new(wf_dir.clone()).unwrap();
    let mut ledger = NudgeLedger::load(&nudge_path).unwrap();
    NudgeEmitter::apply_decision(
        &mut ledger,
        "abc",
        NudgeDecision::Accept,
        7,
        chrono::Utc::now(),
        &|c| {
            mur_core::cmd::workflow::create_draft_workflow_in(
                &wf_store,
                &c.suggested_name,
                &c.title,
                "",
                &c.evidence_session_ids,
            )
        },
    )
    .unwrap();
    ledger.save(&nudge_path).unwrap();

    // 3. Verify Accepted state
    assert!(matches!(
        ledger.get("abc").unwrap().state,
        NudgeState::Accepted
    ));

    // 4. Accepted candidate is not actionable again
    let actionable =
        ledger.filter_actionable(&[cand("abc", "my-workflow")], chrono::Utc::now(), 10);
    assert!(actionable.is_empty());

    // 5. Draft workflow exists
    assert!(wf_store.exists("my-workflow"));
}
