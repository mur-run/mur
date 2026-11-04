use super::*;
use mur_common::skill::event_log::read_events;
use mur_common::skill::stats::SkillStats;
use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind};
use tempfile::tempdir;
use uuid::Uuid;

/// Stub an installed skill under `<home>/skills/<name>/` so the inbox has
/// something to apply signals to.
fn install_skill(home: &Path, name: &str) {
    let skill_dir = home.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("skill.yaml"),
        format!("name: {name}\nversion: 1.0.0\n"),
    )
    .unwrap();
}

fn skill_signal(target_name: &str, kind: SignalKind, actor_native: &str) -> Signal {
    Signal {
        id: Uuid::new_v4(),
        emitted_at: chrono::Utc::now(),
        actor: Actor {
            source: ActorSource::Slack,
            native_id: actor_native.into(),
            display_name: None,
            resolved_user_id: None,
        },
        target: SignalTarget::Skill {
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

/// A frozen-v1 pattern-targeted signal, as an older peer would still emit.
fn legacy_pattern_signal(target_name: &str) -> Signal {
    let mut s = skill_signal(target_name, SignalKind::ExecutionSuccess, "legacy");
    s.target = SignalTarget::Pattern {
        name: target_name.into(),
        scope: Scope::Personal,
    };
    s
}

fn setup(home: &Path) -> Inbox {
    Inbox::new(home.join("inbox")).unwrap()
}

fn success_count(home: &Path, skill: &str) -> u64 {
    SkillStats::load(&SkillStats::path(home, skill))
        .unwrap()
        .map(|s| s.success_count)
        .unwrap_or(0)
}

#[test]
fn apply_skill_success_appends_event_and_updates_stats() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");

    let sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "alice");
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 1, "errors: {:?}", report.errors);

    let events = read_events(&tmp.path().join("skills/s1/events.jsonl")).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        mur_common::skill::event_log::SkillEvent::Execution { ref outcome, .. }
        if outcome == "success"
    ));
    assert_eq!(success_count(tmp.path(), "s1"), 1);
}

#[test]
fn apply_skill_failure_records_failure() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");

    let sig = skill_signal(
        "s1",
        SignalKind::SkillExecutionFailure {
            error: "boom".into(),
        },
        "alice",
    );
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 1, "errors: {:?}", report.errors);

    let stats = SkillStats::load(&SkillStats::path(tmp.path(), "s1"))
        .unwrap()
        .unwrap();
    assert_eq!(stats.failure_count, 1);
    assert_eq!(stats.success_count, 0);
}

#[test]
fn apply_skips_signal_for_uninstalled_skill() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    // no install_skill

    let sig = skill_signal("ghost", SignalKind::SkillExecutionSuccess, "alice");
    let path = inbox.receive(&sig).unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1);
    assert!(report.errors.is_empty());
    assert!(!path.exists(), "skipped signal must be removed");
}

#[test]
fn legacy_pattern_signals_are_acknowledged_and_skipped() {
    // Wire v1 is frozen: older peers still emit `kind: pattern` /
    // `kind: new_draft_pattern`. They must parse, be marked seen, and be
    // removed — never applied, never left to wedge the inbox.
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "p1"); // same name as a skill: must NOT bleed over

    let evidence = legacy_pattern_signal("p1");
    let mut draft = legacy_pattern_signal("draft-x");
    draft.target = SignalTarget::NewDraftPattern {
        payload: Box::new(serde_json::json!({
            "schema": 2,
            "name": "draft-x",
            "description": "legacy draft",
            "content": "use pnpm",
            "tier": "session"
        })),
    };
    draft.kind = SignalKind::NewPatternProposal {
        origin_context: "slack".into(),
    };
    let p1 = inbox.receive(&evidence).unwrap();
    let p2 = inbox.receive(&draft).unwrap();

    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 2);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!p1.exists() && !p2.exists());
    assert_eq!(success_count(tmp.path(), "p1"), 0);

    // Marked seen: a re-emission of the same id is a cheap skip, not a retry.
    std::fs::write(
        inbox.dir.join("2099-01-01T00-00-00-dup.yaml"),
        serde_yaml::to_string(&evidence).unwrap(),
    )
    .unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.skipped, 1);
}

#[test]
fn apply_all_preserves_bad_yaml_in_place() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    let inbox_dir = tmp.path().join("inbox");
    std::fs::write(
        inbox_dir.join("2026-04-18T10-00-00-bad.yaml"),
        "not a signal",
    )
    .unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("parse error"));
    // File should NOT have been removed
    assert!(inbox_dir.join("2026-04-18T10-00-00-bad.yaml").exists());
}

#[test]
fn duplicate_signal_id_is_not_double_counted() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");

    let sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "alice");
    inbox.receive(&sig).unwrap();
    inbox.apply_all().unwrap();

    // Receive the SAME signal UUID again (retry scenario) under a different
    // filename.
    let yaml = serde_yaml::to_string(&sig).unwrap();
    let dup_path = inbox.dir.join("2099-01-01T00-00-00-dup.yaml");
    std::fs::write(&dup_path, yaml).unwrap();
    let report = inbox.apply_all().unwrap();

    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(success_count(tmp.path(), "s1"), 1);
}

#[test]
fn apply_skill_signals_is_an_alias_of_apply_all() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");
    inbox
        .receive(&skill_signal(
            "s1",
            SignalKind::SkillExecutionSuccess,
            "alice",
        ))
        .unwrap();
    let report = inbox.apply_skill_signals().unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(success_count(tmp.path(), "s1"), 1);
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
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "w1");
    sig.sign(&id);
    inbox.receive(&sig).unwrap();
    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 1, "errors: {:?}", report.errors);
    assert!(report.errors.is_empty());
}

#[test]
fn tampered_signed_signal_is_rejected_and_removed() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "w1");
    sig.sign(&id);
    sig.confidence = 0.1; // tamper AFTER signing
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("verification failed"));
    // File removed (permanent failure — no poison retry), stats untouched.
    assert_eq!(
        std::fs::read_dir(tmp.path().join("inbox"))
            .unwrap()
            .filter(|e| is_inbox_yaml(&e.as_ref().unwrap().path()))
            .count(),
        0
    );
    assert_eq!(success_count(tmp.path(), "s1"), 0);
}

#[test]
fn signed_signal_with_non_personal_scope_is_rejected() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");
    let id = agent_fixture(tmp.path(), "w1");

    let mut sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "w1");
    sig.scope = Scope::Team {
        team_id: "ops".into(),
    };
    sig.sign(&id); // signature VALID — the scope claim itself is the violation
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("may not emit"));
}

#[test]
fn signed_signal_without_registered_identity_is_rejected() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    install_skill(tmp.path(), "s1");
    // NO agent_fixture: sign with a key the central store has never seen.
    let rogue = AgentIdentity::generate();

    let mut sig = skill_signal("s1", SignalKind::SkillExecutionSuccess, "ghost");
    sig.sign(&rogue);
    inbox.receive(&sig).unwrap();

    let report = inbox.apply_all().unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("no verifiable identity"));
}

#[test]
fn wire_drops_are_exempt_from_require_sig() {
    let tmp = tempdir().unwrap();
    let inbox = setup(tmp.path());
    let local = skill_signal("s1", SignalKind::SkillExecutionSuccess, "w1");
    let wire = skill_signal("s2", SignalKind::SkillExecutionSuccess, "cmdr");
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
    let inbox = setup(tmp.path());

    // Unsigned tolerated by default, rejected under require.
    let unsigned = skill_signal("s1", SignalKind::SkillExecutionSuccess, "w1");
    assert!(inbox.check_signal_sig(&unsigned, false).is_ok());
    assert!(inbox.check_signal_sig(&unsigned, true).is_err());

    // A signed signal claiming a path-traversal actor never reaches the join.
    let id = AgentIdentity::generate();
    let mut evil = skill_signal("s1", SignalKind::SkillExecutionSuccess, "../w1");
    evil.sign(&id);
    let err = inbox.check_signal_sig(&evil, false).unwrap_err();
    assert!(err.contains("not a valid agent name"));
}
