use super::*;
use crate::store::yaml::YamlStore;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Pattern, Tier};
use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope};
use tempfile::tempdir;
use uuid::Uuid;

/// Build a minimal Pattern with a fixed name, saveable by YamlStore.
fn make_pattern(name: &str) -> Pattern {
    Pattern {
        base: KnowledgeBase {
            name: name.into(),
            description: "test pattern".into(),
            content: Content::Plain("test".into()),
            tier: Tier::Session,
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: Vec::new(),
    }
}

fn signal(target_name: &str, kind: SignalKind, actor_native: &str) -> Signal {
    Signal {
        id: Uuid::new_v4(),
        emitted_at: Utc::now(),
        actor: Actor {
            source: ActorSource::Slack,
            native_id: actor_native.into(),
            display_name: None,
            resolved_user_id: None,
        },
        target: SignalTarget::Pattern {
            name: target_name.into(),
            scope: Scope::Personal,
        },
        kind,
        scope: Scope::Personal,
        confidence: 1.0,
        schema_version: SIGNAL_SCHEMA_VERSION,
        sig: None,
        key_version: 0,
    }
}

fn setup(tmp_dir: &Path) -> (YamlStore, Inbox) {
    let store = YamlStore::new(tmp_dir.join("patterns")).unwrap();
    let inbox = Inbox::new(tmp_dir.join("inbox")).unwrap();
    (store, inbox)
}

#[test]
fn apply_execution_success_updates_contributions_and_global() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    let sig = signal("p1", SignalKind::ExecutionSuccess, "alice");
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors.len(), 0);

    let p = store.get("p1").unwrap();
    assert_eq!(p.evidence.success_signals, 1);
    let contrib = p
        .evidence
        .contributions
        .get("Slack:alice")
        .expect("alice contrib");
    assert_eq!(contrib.success_signals, 1);
    assert_eq!(contrib.override_signals, 0);
}

#[test]
fn apply_override_weights_3x() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    let sig = signal(
        "p1",
        SignalKind::UserOverrideAtBreakpoint { reason: None },
        "alice",
    );
    inbox.receive(&sig).unwrap();
    inbox.apply_all(&store).unwrap();

    let p = store.get("p1").unwrap();
    assert_eq!(p.evidence.override_signals, 3);
    assert_eq!(
        p.evidence
            .contributions
            .get("Slack:alice")
            .unwrap()
            .override_signals,
        3
    );
}

#[test]
fn apply_autofix_weights_1x() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    let sig = signal(
        "p1",
        SignalKind::AutoFixApplied { step: "s".into() },
        "alice",
    );
    inbox.receive(&sig).unwrap();
    inbox.apply_all(&store).unwrap();

    let p = store.get("p1").unwrap();
    assert_eq!(p.evidence.override_signals, 1);
}

#[test]
fn apply_failure_updates_global_only_not_contribution() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    let sig = signal(
        "p1",
        SignalKind::ExecutionFailure {
            error: "db down".into(),
        },
        "alice",
    );
    inbox.receive(&sig).unwrap();
    inbox.apply_all(&store).unwrap();

    let p = store.get("p1").unwrap();
    assert_eq!(p.evidence.failure_signals, 1);
    assert_eq!(p.evidence.success_signals, 0);
    assert_eq!(p.evidence.override_signals, 0);
    // Contribution entry is created for last_seen tracking but no success/override increments
    let contrib = p
        .evidence
        .contributions
        .get("Slack:alice")
        .expect("contrib entry");
    assert_eq!(contrib.success_signals, 0);
    assert_eq!(contrib.override_signals, 0);
}

#[test]
fn apply_skips_signal_for_nonexistent_pattern() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    // Note: do NOT save any pattern

    let sig = signal("nonexistent", SignalKind::ExecutionSuccess, "alice");
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.errors.len(), 0);
}

#[test]
fn apply_creates_new_draft_pattern() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    let pat = make_pattern("draft-x");
    let sig = Signal {
        id: Uuid::new_v4(),
        emitted_at: Utc::now(),
        actor: Actor {
            source: ActorSource::Slack,
            native_id: "alice".into(),
            display_name: None,
            resolved_user_id: None,
        },
        target: SignalTarget::NewDraftPattern {
            payload: Box::new(pat),
        },
        kind: SignalKind::NewPatternProposal {
            origin_context: "chat".into(),
        },
        scope: Scope::Personal,
        confidence: 1.0,
        schema_version: SIGNAL_SCHEMA_VERSION,
        sig: None,
        key_version: 0,
    };
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
    // Pattern was created as draft
    assert!(store.exists("draft-x"));
}

#[test]
fn apply_skips_new_draft_pattern_when_pattern_already_exists() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    // Pre-existing pattern with the same name
    store.save(&make_pattern("draft-x")).unwrap();
    let pat = make_pattern("draft-x");
    let sig = Signal {
        id: Uuid::new_v4(),
        emitted_at: Utc::now(),
        actor: Actor {
            source: ActorSource::Slack,
            native_id: "alice".into(),
            display_name: None,
            resolved_user_id: None,
        },
        target: SignalTarget::NewDraftPattern {
            payload: Box::new(pat),
        },
        kind: SignalKind::NewPatternProposal {
            origin_context: "chat".into(),
        },
        scope: Scope::Personal,
        confidence: 1.0,
        schema_version: SIGNAL_SCHEMA_VERSION,
        sig: None,
        key_version: 0,
    };
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.skipped, 1);
    assert_eq!(report.applied, 0);
}

#[test]
fn apply_all_preserves_bad_yaml_in_place() {
    let tmp = tempdir().unwrap();
    let inbox_dir = tmp.path().join("inbox");
    let (store, inbox) = setup(tmp.path());
    // Write a bogus YAML file directly (drop store to avoid unused warning)
    let _ = store;
    std::fs::write(
        inbox_dir.join("2026-04-18T10-00-00-bad.yaml"),
        "not a signal",
    )
    .unwrap();
    let store2 = YamlStore::new(tmp.path().join("patterns")).unwrap();
    let report = inbox.apply_all(&store2).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("parse error"));
    // File should NOT have been removed
    assert!(inbox_dir.join("2026-04-18T10-00-00-bad.yaml").exists());
}

#[test]
fn two_actors_contributions_tracked_separately() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    inbox
        .receive(&signal("p1", SignalKind::ExecutionSuccess, "alice"))
        .unwrap();
    inbox
        .receive(&signal("p1", SignalKind::ExecutionSuccess, "alice"))
        .unwrap();
    inbox
        .receive(&signal(
            "p1",
            SignalKind::UserOverrideAtBreakpoint { reason: None },
            "bob",
        ))
        .unwrap();
    inbox.apply_all(&store).unwrap();

    let p = store.get("p1").unwrap();
    assert_eq!(p.evidence.success_signals, 2);
    assert_eq!(p.evidence.override_signals, 3);
    let alice = p.evidence.contributions.get("Slack:alice").unwrap();
    let bob = p.evidence.contributions.get("Slack:bob").unwrap();
    assert_eq!(alice.success_signals, 2);
    assert_eq!(bob.override_signals, 3);
}

#[test]
fn duplicate_signal_id_is_not_double_counted() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();

    // First receive + apply
    let sig = signal("p1", SignalKind::ExecutionSuccess, "alice");
    inbox.receive(&sig).unwrap();
    inbox.apply_all(&store).unwrap();

    // Receive the SAME signal UUID again (retry scenario) with different filename
    // by writing directly with a different timestamp prefix
    let yaml = serde_yaml::to_string(&sig).unwrap();
    let dup_path = inbox.dir.join("2099-01-01T00-00-00-dup.yaml");
    std::fs::write(&dup_path, yaml).unwrap();
    let report = inbox.apply_all(&store).unwrap();

    // The duplicate should be skipped (not applied again)
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1);

    let p = store.get("p1").unwrap();
    // Still only 1 success signal — not incremented twice
    assert_eq!(p.evidence.success_signals, 1);
}

#[test]
fn skill_execution_signal_appends_event_and_updates_stats() {
    use mur_common::skill::event_log::read_events;
    use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind, SignalTarget};
    use uuid::Uuid;

    let dir = tempdir().unwrap();
    // Stub skill.yaml so the skill exists
    let skill_dir = dir.path().join("skills/test-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("skill.yaml"),
        "name: test-skill\nversion: 1.0.0\n",
    )
    .unwrap();

    let inbox_dir = dir.path().join("inbox");
    let inbox = Inbox::new_with_mur_home(&inbox_dir, dir.path()).unwrap();

    let signal = mur_common::Signal {
        id: Uuid::new_v4(),
        schema_version: SIGNAL_SCHEMA_VERSION,
        emitted_at: chrono::Utc::now(),
        actor: Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "a".into(),
            display_name: None,
            resolved_user_id: None,
        },
        target: SignalTarget::Skill {
            name: "test-skill".into(),
            scope: Scope::Personal,
        },
        kind: SignalKind::SkillExecutionSuccess,
        scope: Scope::Personal,
        confidence: 1.0,
        sig: None,
        key_version: 0,
    };
    inbox.receive(&signal).unwrap();
    let report = inbox.apply_skill_signals().unwrap();
    assert_eq!(report.applied, 1);

    let events = read_events(&dir.path().join("skills/test-skill/events.jsonl")).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        mur_common::skill::event_log::SkillEvent::Execution { ref outcome, .. }
        if outcome == "success"
    ));
}

// ── P2c-2 signature gate ─────────────────────────────────────────────

use mur_common::identity::AgentIdentity;

/// Register agent `name` under `<home>/agents/` with a real keypair.
fn agent_fixture(home: &Path, name: &str) -> AgentIdentity {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let id = AgentIdentity::generate();
    id.save(&dir).unwrap();
    id
}

#[test]
fn signed_signal_verifies_and_applies() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
    sig.sign(&id);
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 1, "errors: {:?}", report.errors);
    assert!(report.errors.is_empty());
}

#[test]
fn tampered_signed_signal_is_rejected_and_removed() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
    sig.sign(&id);
    sig.confidence = 0.1; // tamper AFTER signing
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("verification failed"));
    // File removed (permanent failure — no poison retry), pattern untouched.
    assert_eq!(
        std::fs::read_dir(tmp.path().join("inbox"))
            .unwrap()
            .filter(|e| is_inbox_yaml(&e.as_ref().unwrap().path()))
            .count(),
        0
    );
    assert_eq!(store.get("p1").unwrap().evidence.success_signals, 0);
}

#[test]
fn signed_signal_with_non_personal_scope_is_rejected() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
    sig.scope = Scope::Team {
        team_id: "ops".into(),
    };
    sig.sign(&id); // signature VALID — the scope claim itself is the violation
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("may not emit"));
}

#[test]
fn signed_signal_without_registered_identity_is_rejected() {
    let tmp = tempdir().unwrap();
    let (store, inbox) = setup(tmp.path());
    store.save(&make_pattern("p1")).unwrap();
    // NO agent_fixture: sign with a key the central store has never seen.
    let rogue = AgentIdentity::generate();

    let mut sig = signal("p1", SignalKind::ExecutionSuccess, "ghost");
    sig.sign(&rogue);
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all(&store).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("no verifiable identity"));
}

#[test]
fn wire_drops_are_exempt_from_require_sig() {
    let tmp = tempdir().unwrap();
    let (_store, inbox) = setup(tmp.path());
    let local = signal("p1", SignalKind::ExecutionSuccess, "w1");
    let wire = signal("p2", SignalKind::ExecutionSuccess, "cmdr");
    inbox.receive(&local).unwrap();
    inbox.receive_wire(&wire).unwrap();

    // The wire drop lands under wire/, not the main dir.
    assert!(inbox.wire_dir().join(signal_file_name(&wire)).exists());

    let pairs = inbox.scan(true).unwrap();
    assert_eq!(pairs.len(), 2);
    let require_for = |id: &uuid::Uuid| {
        pairs
            .iter()
            .find(|(p, _)| p.to_string_lossy().contains(&id.to_string()))
            .unwrap()
            .1
    };
    assert!(
        require_for(&local.id),
        "local drop must require a signature"
    );
    assert!(!require_for(&wire.id), "wire drop must be exempt");
}

#[test]
fn require_mode_rejects_unsigned_and_traversal_actor_names() {
    let tmp = tempdir().unwrap();
    let (_store, inbox) = setup(tmp.path());

    // Unsigned tolerated by default, rejected under require.
    let unsigned = signal("p1", SignalKind::ExecutionSuccess, "w1");
    assert!(inbox.check_signal_sig(&unsigned, false).is_ok());
    assert!(inbox.check_signal_sig(&unsigned, true).is_err());

    // A signed signal claiming a path-traversal actor never reaches the join.
    let id = AgentIdentity::generate();
    let mut evil = signal("p1", SignalKind::ExecutionSuccess, "../w1");
    evil.sign(&id);
    let err = inbox.check_signal_sig(&evil, false).unwrap_err();
    assert!(err.contains("not a valid agent name"));
}
