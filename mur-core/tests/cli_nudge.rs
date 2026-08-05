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

#[test]
fn phase2_end_to_end_deliver_ack_drain() {
    let mur = tempfile::tempdir().unwrap();

    // 1. Create a companion-enabled agent profile.
    let mut prof = mur_common::agent::AgentProfile::default_for_tests();
    prof.companion.enabled = true;
    let agent_dir = mur.path().join("agents/test-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&prof).unwrap(),
    )
    .unwrap();

    // 2. Seed a ledger with a surfaced candidate (snapshot present).
    let c = cand("phase2", "phase2-workflow");
    let mut ledger = NudgeLedger::default();
    NudgeEmitter::emit_pending(&mut ledger, std::slice::from_ref(&c), chrono::Utc::now());
    ledger
        .save(&NudgeLedger::default_path_in(mur.path()))
        .unwrap();

    // 3. Deliver the candidate to the agent inbox.
    let n = mur_core::nudge::companion::deliver_nudges_to_companions(
        mur.path(),
        std::slice::from_ref(&c),
        "en",
    )
    .unwrap();
    assert_eq!(n, 1);
    let inbox_file = agent_dir.join("companion/inbox/nudge_phase2.md");
    assert!(inbox_file.exists());

    // 4. Simulate GUI ack: rewrite response to "good".
    let content = std::fs::read_to_string(&inbox_file).unwrap();
    let new_content = content.replace("<unset>", "good");
    std::fs::write(&inbox_file, &new_content).unwrap();

    // 5. Drain → draft created, ledger Accepted, inbox file consumed.
    let wf_dir = mur.path().join("workflows");
    let wf_store = mur_core::store::workflow_yaml::WorkflowYamlStore::new(wf_dir.clone()).unwrap();
    let applied = mur_core::nudge::companion::drain_nudge_responses_in(mur.path(), &|cand| {
        mur_core::cmd::workflow::create_draft_workflow_in(
            &wf_store,
            &cand.suggested_name,
            &cand.title,
            "",
            &cand.evidence_session_ids,
        )
    })
    .unwrap();
    assert_eq!(applied, 1);

    // Verify ledger → Accepted.
    let ledger = NudgeLedger::load(&NudgeLedger::default_path_in(mur.path())).unwrap();
    assert!(matches!(
        ledger.get("phase2").unwrap().state,
        NudgeState::Accepted
    ));

    // Verify draft workflow created.
    assert!(wf_store.exists("phase2-workflow"));

    // Verify inbox file consumed.
    assert!(!inbox_file.exists());

    // 6. Re-deliver same candidate → write_nudge_inbox is idempotent
    //    but the consumed file was removed. Re-deliver recreates it;
    //    the ledger filter in record_nudges_for_candidates prevents
    //    accepted candidates from being surfaced again upstream.
    let n2 = mur_core::nudge::companion::deliver_nudges_to_companions(
        mur.path(),
        std::slice::from_ref(&c),
        "en",
    )
    .unwrap();
    assert_eq!(n2, 1); // deliver doesn't check ledger; file was consumed
    // But filter_actionable excludes it:
    let actionable = ledger.filter_actionable(&[c], chrono::Utc::now(), 10);
    assert!(actionable.is_empty());
}
